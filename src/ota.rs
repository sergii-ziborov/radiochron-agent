use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::HttpEndpoint;
use crate::transport::TlsConnector;
use crate::update_fs::{atomic_json, digest_file, hex, replace_file};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtaManifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default = "default_health_timeout")]
    pub health_timeout_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingUpdate {
    schema_version: u32,
    device_id: String,
    profile_revision: u64,
    version: String,
    artifact_path: PathBuf,
    target_path: PathBuf,
    sha256: String,
    health_timeout_seconds: u64,
    phase: String,
    attempts: u8,
}

pub fn stage(
    manifest: &OtaManifest,
    fleet_state_dir: &Path,
    connector: &TlsConnector,
    device_id: &str,
    profile_revision: u64,
) -> anyhow::Result<()> {
    validate_version(&manifest.version)?;
    validate_digest(&manifest.sha256)?;
    let pending_path = fleet_state_dir.join("pending-update.json");
    if let Ok(contents) = std::fs::read(&pending_path) {
        if let Ok(existing) = serde_json::from_slice::<PendingUpdate>(&contents) {
            if existing.version == manifest.version
                && existing.profile_revision == profile_revision
                && existing.sha256.eq_ignore_ascii_case(&manifest.sha256)
            {
                return Ok(());
            }
        }
    }
    let root = fleet_state_dir.join("updates").join(&manifest.version);
    std::fs::create_dir_all(&root)?;
    crate::private_fs::restrict_directory(&root)?;
    let artifact = root.join("radiochron-agent.bin");
    if artifact.exists() && digest_file(&artifact)? == manifest.sha256.to_ascii_lowercase() {
        crate::private_fs::restrict_executable(&artifact)?;
        write_pending(
            fleet_state_dir,
            manifest,
            artifact,
            device_id,
            profile_revision,
        )?;
        return Ok(());
    }
    let endpoint = HttpEndpoint::parse(&manifest.url, "/")?;
    if !endpoint.tls && !matches!(endpoint.host.as_str(), "127.0.0.1" | "::1" | "localhost") {
        bail!("OTA artifact URL requires HTTPS outside loopback");
    }
    let response = endpoint.request("GET", &[], &[], Duration::from_secs(60), connector)?;
    if response.status != 200 {
        bail!("OTA artifact download returned HTTP {}", response.status);
    }
    let actual = hex(&Sha256::digest(&response.body));
    if !actual.eq_ignore_ascii_case(&manifest.sha256) {
        bail!(
            "OTA artifact sha256 mismatch: expected {}, got {actual}",
            manifest.sha256
        );
    }
    let temp = artifact.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, &response.body)?;
    crate::private_fs::restrict_executable(&temp)?;
    replace_file(&temp, &artifact)?;
    write_pending(
        fleet_state_dir,
        manifest,
        artifact,
        device_id,
        profile_revision,
    )
}

pub fn mark_healthy(spool_dir: &Path) -> anyhow::Result<()> {
    let state_dir = spool_dir.join("state").join("fleet");
    let path = state_dir.join("pending-update.json");
    let Ok(contents) = std::fs::read(&path) else {
        return Ok(());
    };
    let mut pending: PendingUpdate = serde_json::from_slice(&contents)?;
    if pending.phase == "awaiting_health" {
        pending.phase = "healthy".to_string();
        atomic_json(&path, &pending)?;
    }
    Ok(())
}

fn write_pending(
    state_dir: &Path,
    manifest: &OtaManifest,
    artifact_path: PathBuf,
    device_id: &str,
    profile_revision: u64,
) -> anyhow::Result<()> {
    let target_path = std::env::current_exe().context("locate running agent executable")?;
    atomic_json(
        &state_dir.join("pending-update.json"),
        &PendingUpdate {
            schema_version: 1,
            device_id: device_id.to_string(),
            profile_revision,
            version: manifest.version.clone(),
            artifact_path,
            target_path,
            sha256: manifest.sha256.to_ascii_lowercase(),
            health_timeout_seconds: manifest.health_timeout_seconds,
            phase: "pending".to_string(),
            attempts: 0,
        },
    )
}

fn validate_version(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        bail!("OTA version contains unsafe path characters");
    }
    Ok(())
}

fn validate_digest(value: &str) -> anyhow::Result<()> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        bail!("OTA sha256 must contain exactly 64 hexadecimal characters");
    }
    Ok(())
}

const fn default_health_timeout() -> u64 {
    120
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_digest_are_path_safe() {
        assert!(validate_version("1.2.3-arm64").is_ok());
        assert!(validate_version("../escape").is_err());
        assert!(validate_digest(&"a".repeat(64)).is_ok());
        assert!(validate_digest("abc").is_err());
    }
}
