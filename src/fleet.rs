use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::http::HttpEndpoint;
use crate::metrics::Metrics;
use crate::ota::{self, OtaManifest};
use crate::transport::TlsConnector;

#[derive(Debug, Serialize)]
struct EnrollmentRequest<'a> {
    device_id: &'a str,
    boot_id: &'a str,
    agent_version: &'static str,
    platform: &'static str,
}

#[derive(Debug, Deserialize)]
struct EnrollmentResponse {
    device_token: String,
    fleet_public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FleetIdentity {
    device_token: String,
    fleet_public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredPayload {
    pub schema_version: u32,
    pub device_id: String,
    pub profile_id: String,
    pub revision: u64,
    pub issued_at_epoch_seconds: i64,
    pub config: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ota: Option<OtaManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignedDesired {
    payload: DesiredPayload,
    signature: String,
}

#[derive(Serialize)]
struct Heartbeat<'a> {
    device_id: &'a str,
    boot_id: &'a str,
    agent_version: &'static str,
    config_revision: Option<u64>,
    observed_at_epoch_seconds: i64,
    metrics: crate::metrics::MetricsSnapshot,
}

pub struct FleetClient {
    base_url: String,
    enroll_token: Option<String>,
    device_id: String,
    boot_id: String,
    state_dir: PathBuf,
    identity: Option<FleetIdentity>,
    desired: Option<SignedDesired>,
    connector: TlsConnector,
    poll_interval: Duration,
    last_poll: Option<Instant>,
}

impl FleetClient {
    pub fn open(config: &Config, connector: TlsConnector) -> anyhow::Result<Option<Self>> {
        let Some(base_url) = config.fleet_url.as_deref() else {
            return Ok(None);
        };
        let base_url = base_url.trim_end_matches('/').to_string();
        let endpoint = HttpEndpoint::parse(&base_url, "/")?;
        require_secure_endpoint(&endpoint)?;
        let state_dir = config.spool_dir.join("state").join("fleet");
        std::fs::create_dir_all(&state_dir)?;
        crate::private_fs::restrict_directory(&state_dir)?;
        let identity = read_json(&state_dir.join("identity.json"))?;
        let desired = read_json(&state_dir.join("desired.json"))?;
        Ok(Some(Self {
            base_url,
            enroll_token: config.fleet_enroll_token.clone(),
            device_id: config.device_id.clone(),
            boot_id: config.boot_id.clone(),
            state_dir,
            identity,
            desired,
            connector,
            poll_interval: config.fleet_poll_interval,
            last_poll: None,
        }))
    }

    pub fn apply_cached(&self, config: &mut Config) -> anyhow::Result<()> {
        if let Some(desired) = &self.desired {
            self.verify(desired)?;
            config.apply_profile(&desired.payload.config)?;
        }
        Ok(())
    }

    pub fn bootstrap(&mut self, config: &mut Config) -> anyhow::Result<()> {
        self.ensure_enrolled()?;
        if let Some(desired) = self.fetch_desired()? {
            config.apply_profile(&desired.payload.config)?;
            self.stage_ota(&desired.payload)?;
            self.store_desired(desired)?;
        }
        self.last_poll = Some(Instant::now());
        Ok(())
    }

    /// Returns true when a new config revision was installed and a clean
    /// service-manager restart is required to rebuild recorder/exporters.
    pub fn tick(&mut self, config: &mut Config, metrics: &Arc<Metrics>) -> anyhow::Result<bool> {
        if self
            .last_poll
            .is_some_and(|last| last.elapsed() < self.poll_interval)
        {
            return Ok(false);
        }
        self.ensure_enrolled()?;
        self.heartbeat(metrics)?;
        let current_revision = self.desired.as_ref().map(|item| item.payload.revision);
        let Some(desired) = self.fetch_desired()? else {
            self.last_poll = Some(Instant::now());
            return Ok(false);
        };
        let changed = current_revision != Some(desired.payload.revision);
        config.apply_profile(&desired.payload.config)?;
        self.stage_ota(&desired.payload)?;
        self.store_desired(desired)?;
        self.last_poll = Some(Instant::now());
        Ok(changed)
    }

    fn ensure_enrolled(&mut self) -> anyhow::Result<()> {
        if self.identity.is_some() {
            return Ok(());
        }
        let token = self
            .enroll_token
            .as_deref()
            .context("fleet enrollment token is required for first enrollment")?;
        let endpoint = self.endpoint("/v1/enroll")?;
        let body = serde_json::to_vec(&EnrollmentRequest {
            device_id: &self.device_id,
            boot_id: &self.boot_id,
            agent_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
        })?;
        let authorization = format!("Bearer {token}");
        let response = endpoint.request(
            "POST",
            &[
                ("Content-Type", "application/json"),
                ("Authorization", &authorization),
            ],
            &body,
            Duration::from_secs(10),
            &self.connector,
        )?;
        if response.status != 201 {
            bail!("fleet enrollment returned HTTP {}", response.status);
        }
        let enrolled: EnrollmentResponse = serde_json::from_slice(&response.body)?;
        let identity = FleetIdentity {
            device_token: enrolled.device_token,
            fleet_public_key: enrolled.fleet_public_key,
        };
        atomic_json(&self.state_dir.join("identity.json"), &identity)?;
        self.identity = Some(identity);
        Ok(())
    }

    fn fetch_desired(&self) -> anyhow::Result<Option<SignedDesired>> {
        let identity = self.identity.as_ref().context("device is not enrolled")?;
        let endpoint = self.endpoint(&format!("/v1/devices/{}/desired", self.device_id))?;
        let authorization = format!("Bearer {}", identity.device_token);
        let response = endpoint.request(
            "GET",
            &[("Authorization", &authorization)],
            &[],
            Duration::from_secs(10),
            &self.connector,
        )?;
        if response.status == 204 {
            return Ok(None);
        }
        if response.status != 200 {
            bail!(
                "fleet desired-state request returned HTTP {}",
                response.status
            );
        }
        let desired: SignedDesired = serde_json::from_slice(&response.body)?;
        self.verify(&desired)?;
        if desired.payload.device_id != self.device_id {
            bail!("signed desired state targets another device");
        }
        Ok(Some(desired))
    }

    fn heartbeat(&self, metrics: &Arc<Metrics>) -> anyhow::Result<()> {
        let identity = self.identity.as_ref().context("device is not enrolled")?;
        let endpoint = self.endpoint(&format!("/v1/devices/{}/heartbeat", self.device_id))?;
        let authorization = format!("Bearer {}", identity.device_token);
        let body = serde_json::to_vec(&Heartbeat {
            device_id: &self.device_id,
            boot_id: &self.boot_id,
            agent_version: env!("CARGO_PKG_VERSION"),
            config_revision: self.desired.as_ref().map(|item| item.payload.revision),
            observed_at_epoch_seconds: radiochron::time::now_epoch_seconds(),
            metrics: metrics.snapshot(),
        })?;
        let response = endpoint.request(
            "POST",
            &[
                ("Content-Type", "application/json"),
                ("Authorization", &authorization),
            ],
            &body,
            Duration::from_secs(10),
            &self.connector,
        )?;
        if !matches!(response.status, 200 | 202 | 204) {
            bail!("fleet heartbeat returned HTTP {}", response.status);
        }
        Ok(())
    }

    fn verify(&self, desired: &SignedDesired) -> anyhow::Result<()> {
        let public = &self
            .identity
            .as_ref()
            .context("fleet identity is missing")?
            .fleet_public_key;
        let public: [u8; 32] = URL_SAFE_NO_PAD
            .decode(public)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("fleet public key must contain 32 bytes"))?;
        let key = VerifyingKey::from_bytes(&public)?;
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(&desired.signature)?)?;
        key.verify(&serde_json::to_vec(&desired.payload)?, &signature)
            .context("fleet desired-state signature is invalid")
    }

    fn stage_ota(&self, desired: &DesiredPayload) -> anyhow::Result<()> {
        if let Some(manifest) = &desired.ota {
            ota::stage(
                manifest,
                &self.state_dir,
                &self.connector,
                &self.device_id,
                desired.revision,
            )?;
        }
        Ok(())
    }

    fn store_desired(&mut self, desired: SignedDesired) -> anyhow::Result<()> {
        atomic_json(&self.state_dir.join("desired.json"), &desired)?;
        self.desired = Some(desired);
        Ok(())
    }

    fn endpoint(&self, path: &str) -> anyhow::Result<HttpEndpoint> {
        let endpoint = HttpEndpoint::parse(&format!("{}{path}", self.base_url), path)?;
        require_secure_endpoint(&endpoint)?;
        Ok(endpoint)
    }
}

fn require_secure_endpoint(endpoint: &HttpEndpoint) -> anyhow::Result<()> {
    if endpoint.tls || matches!(endpoint.host.as_str(), "127.0.0.1" | "::1" | "localhost") {
        Ok(())
    } else {
        bail!("fleet and OTA endpoints require HTTPS outside loopback")
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(
            serde_json::from_slice(&contents)
                .with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn atomic_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(&temp, bytes)?;
    crate::private_fs::restrict_file(&temp)?;
    crate::update_fs::replace_file(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_plaintext_fleet_traffic_off_loopback() {
        let endpoint = HttpEndpoint::parse("http://fleet.example/v1", "/").unwrap();
        assert!(require_secure_endpoint(&endpoint).is_err());
        let loopback = HttpEndpoint::parse("http://127.0.0.1:8080/v1", "/").unwrap();
        assert!(require_secure_endpoint(&loopback).is_ok());
    }
}
