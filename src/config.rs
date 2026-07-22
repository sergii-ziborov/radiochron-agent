use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use radiochron::chronicle::ClockQuality;
use radiochron::connectivity::ConnectivityConfig;

#[derive(Debug, Clone)]
pub struct Config {
    pub device_id: String,
    pub boot_id: String,
    pub clock_quality: ClockQuality,
    pub spool_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub poll_interval: Duration,
    pub connectivity_interval: Duration,
    pub connectivity: Option<ConnectivityConfig>,
    pub mqtt_url: Option<String>,
    pub mqtt_topic: String,
    pub otlp_endpoint: Option<String>,
    pub prometheus_bind: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let device_id = env_nonempty("RADIOCHRON_DEVICE_ID")
            .or_else(native_device_id)
            .context("set RADIOCHRON_DEVICE_ID (no stable machine identity was found)")?;
        let boot_id = env_nonempty("RADIOCHRON_BOOT_ID")
            .or_else(native_boot_id)
            .unwrap_or_else(|| {
                format!(
                    "process-{}-{}",
                    std::process::id(),
                    radiochron::time::now_epoch_seconds()
                )
            });
        let clock_quality = match env_nonempty("RADIOCHRON_CLOCK_QUALITY").as_deref() {
            Some("synchronized") => ClockQuality::Synchronized,
            Some("unsynchronized") => ClockQuality::Unsynchronized,
            Some("unknown") | None => ClockQuality::Unknown,
            Some(value) => bail!(
                "RADIOCHRON_CLOCK_QUALITY must be synchronized, unsynchronized, or unknown; got {value}"
            ),
        };
        let poll_interval = Duration::from_secs(env_u64("RADIOCHRON_POLL_SECONDS", 5)?);
        if poll_interval.is_zero() {
            bail!("RADIOCHRON_POLL_SECONDS must be greater than zero");
        }
        let connectivity_interval =
            Duration::from_secs(env_u64("RADIOCHRON_CONNECTIVITY_SECONDS", 30)?);
        let dns_name = env_nonempty("RADIOCHRON_DNS_NAME");
        let tcp_target = env_nonempty("RADIOCHRON_TCP_TARGET");
        let internet_target = env_nonempty("RADIOCHRON_INTERNET_TARGET");
        let connectivity_timeout =
            Duration::from_millis(env_u64("RADIOCHRON_CONNECTIVITY_TIMEOUT_MS", 3_000)?);
        let connectivity = (dns_name.is_some()
            || tcp_target.is_some()
            || internet_target.is_some())
        .then_some(ConnectivityConfig {
            dns_name,
            tcp_target,
            internet_target,
            timeout: connectivity_timeout,
        });

        Ok(Self {
            device_id: device_id.clone(),
            boot_id,
            clock_quality,
            spool_dir: env_nonempty("RADIOCHRON_SPOOL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_spool_dir),
            spool_max_bytes: {
                let value = env_u64("RADIOCHRON_SPOOL_MAX_BYTES", 64 * 1024 * 1024)?;
                if value == 0 {
                    bail!("RADIOCHRON_SPOOL_MAX_BYTES must be greater than zero");
                }
                value
            },
            poll_interval,
            connectivity_interval,
            connectivity,
            mqtt_url: env_nonempty("RADIOCHRON_MQTT_URL"),
            mqtt_topic: env_nonempty("RADIOCHRON_MQTT_TOPIC")
                .unwrap_or_else(|| format!("radiochron/{device_id}/chronicle")),
            otlp_endpoint: env_nonempty("RADIOCHRON_OTLP_ENDPOINT"),
            prometheus_bind: env_nonempty("RADIOCHRON_PROMETHEUS_BIND"),
        })
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_u64(name: &str, default: u64) -> anyhow::Result<u64> {
    match env_nonempty(name) {
        Some(value) => value
            .parse()
            .with_context(|| format!("{name} must be an unsigned integer")),
        None => Ok(default),
    }
}

fn native_device_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Ok(value) = std::fs::read_to_string("/etc/machine-id") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    env_nonempty("HOSTNAME").or_else(|| env_nonempty("COMPUTERNAME"))
}

fn native_boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Ok(value) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn default_spool_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/lib/radiochron-agent/spool")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("radiochron-agent")
            .join("spool")
    }
}
