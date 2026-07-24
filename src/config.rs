use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use radiochron::chronicle::ClockQuality;
use radiochron::connectivity::ConnectivityConfig;

use crate::ble_collector::BleOptions;
use crate::transport::TlsConfig;

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
    pub ble: Option<BleOptions>,
    pub mqtt_url: Option<String>,
    pub mqtt_topic: String,
    pub otlp_endpoint: Option<String>,
    pub prometheus_bind: Option<String>,
    pub tls: TlsConfig,
    pub fleet_url: Option<String>,
    pub fleet_enroll_token: Option<String>,
    pub fleet_poll_interval: Duration,
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
        let captive_portal_url = env_nonempty("RADIOCHRON_CAPTIVE_PORTAL_URL");
        let tls_target = env_nonempty("RADIOCHRON_TLS_TARGET");
        let quality_target = env_nonempty("RADIOCHRON_QUALITY_TARGET");
        let connectivity_timeout =
            Duration::from_millis(env_u64("RADIOCHRON_CONNECTIVITY_TIMEOUT_MS", 3_000)?);
        let connectivity = (dns_name.is_some()
            || tcp_target.is_some()
            || internet_target.is_some()
            || captive_portal_url.is_some()
            || tls_target.is_some()
            || quality_target.is_some())
        .then_some(ConnectivityConfig {
            dns_name,
            tcp_target,
            internet_target,
            captive_portal_url,
            captive_portal_expected_status: env_u64(
                "RADIOCHRON_CAPTIVE_PORTAL_EXPECTED_STATUS",
                204,
            )?
            .try_into()
            .context("RADIOCHRON_CAPTIVE_PORTAL_EXPECTED_STATUS must fit a u16")?,
            tls_target,
            quality_target,
            quality_attempts: env_u64("RADIOCHRON_QUALITY_ATTEMPTS", 4)?
                .try_into()
                .context("RADIOCHRON_QUALITY_ATTEMPTS must fit a u8")?,
            timeout: connectivity_timeout,
        });
        let ble_scan_seconds = env_u64("RADIOCHRON_BLE_SCAN_SECONDS", 0)?;
        let ble_window_ms = env_u64("RADIOCHRON_BLE_WINDOW_MS", 4_000)?;
        if !(500..=30_000).contains(&ble_window_ms) {
            bail!("RADIOCHRON_BLE_WINDOW_MS must be between 500 and 30000");
        }
        let ble_sensor_is_moving = env_bool("RADIOCHRON_BLE_SENSOR_MOVING", false)?;
        let ble = (ble_scan_seconds > 0).then(|| BleOptions {
            interval: Duration::from_secs(ble_scan_seconds),
            window: Duration::from_millis(ble_window_ms),
            sensor_id: device_id.clone(),
            zone: env_nonempty("RADIOCHRON_BLE_ZONE"),
            movement_session: env_nonempty("RADIOCHRON_BLE_MOVEMENT_SESSION"),
            sensor_is_moving: ble_sensor_is_moving,
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
            ble,
            mqtt_url: env_nonempty("RADIOCHRON_MQTT_URL"),
            mqtt_topic: env_nonempty("RADIOCHRON_MQTT_TOPIC")
                .unwrap_or_else(|| format!("radiochron/{device_id}/chronicle")),
            otlp_endpoint: env_nonempty("RADIOCHRON_OTLP_ENDPOINT"),
            prometheus_bind: env_nonempty("RADIOCHRON_PROMETHEUS_BIND"),
            tls: TlsConfig {
                ca_file: env_nonempty("RADIOCHRON_TLS_CA_FILE").map(PathBuf::from),
                client_cert_file: env_nonempty("RADIOCHRON_TLS_CLIENT_CERT_FILE")
                    .map(PathBuf::from),
                client_key_file: env_nonempty("RADIOCHRON_TLS_CLIENT_KEY_FILE").map(PathBuf::from),
                server_name: env_nonempty("RADIOCHRON_TLS_SERVER_NAME"),
            },
            fleet_url: env_nonempty("RADIOCHRON_FLEET_URL"),
            fleet_enroll_token: env_nonempty("RADIOCHRON_FLEET_ENROLL_TOKEN"),
            fleet_poll_interval: Duration::from_secs(env_u64("RADIOCHRON_FLEET_POLL_SECONDS", 60)?),
        })
    }

    pub fn apply_profile(&mut self, profile: &serde_json::Value) -> anyhow::Result<()> {
        let object = profile
            .as_object()
            .context("fleet profile config must be a JSON object")?;
        if let Some(value) = object
            .get("poll_seconds")
            .and_then(serde_json::Value::as_u64)
        {
            if value == 0 {
                bail!("fleet poll_seconds must be greater than zero");
            }
            self.poll_interval = Duration::from_secs(value);
        }
        if let Some(value) = object
            .get("connectivity_seconds")
            .and_then(serde_json::Value::as_u64)
        {
            if value == 0 {
                bail!("fleet connectivity_seconds must be greater than zero");
            }
            self.connectivity_interval = Duration::from_secs(value);
        }
        let string = |name: &str| -> anyhow::Result<Option<String>> {
            match object.get(name) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(value) => value
                    .as_str()
                    .map(|value| Some(value.to_string()))
                    .with_context(|| format!("fleet {name} must be a string or null")),
            }
        };
        for (name, target) in [
            ("mqtt_url", &mut self.mqtt_url),
            ("otlp_endpoint", &mut self.otlp_endpoint),
            ("prometheus_bind", &mut self.prometheus_bind),
        ] {
            if object.contains_key(name) {
                *target = string(name)?;
            }
        }

        let mut connectivity = self.connectivity.clone().unwrap_or_default();
        let mut connectivity_changed = false;
        for (name, target) in [
            ("dns_name", &mut connectivity.dns_name),
            ("tcp_target", &mut connectivity.tcp_target),
            ("internet_target", &mut connectivity.internet_target),
            ("captive_portal_url", &mut connectivity.captive_portal_url),
            ("tls_target", &mut connectivity.tls_target),
            ("quality_target", &mut connectivity.quality_target),
        ] {
            if object.contains_key(name) {
                *target = string(name)?;
                connectivity_changed = true;
            }
        }
        if let Some(value) = object
            .get("quality_attempts")
            .and_then(serde_json::Value::as_u64)
        {
            connectivity.quality_attempts = value
                .try_into()
                .context("fleet quality_attempts must fit a u8")?;
            connectivity_changed = true;
        }
        if connectivity_changed {
            self.connectivity = Some(connectivity);
        }
        if object.contains_key("ble_scan_seconds") {
            let seconds = object
                .get("ble_scan_seconds")
                .and_then(serde_json::Value::as_u64)
                .context("fleet ble_scan_seconds must be an unsigned integer")?;
            if seconds == 0 {
                self.ble = None;
            } else {
                let options = self.ble.get_or_insert_with(|| BleOptions {
                    interval: Duration::from_secs(seconds),
                    window: Duration::from_millis(4_000),
                    sensor_id: self.device_id.clone(),
                    zone: None,
                    movement_session: None,
                    sensor_is_moving: false,
                });
                options.interval = Duration::from_secs(seconds);
            }
        }
        if object.contains_key("ble_window_ms") {
            let window_ms = object
                .get("ble_window_ms")
                .and_then(serde_json::Value::as_u64)
                .context("fleet ble_window_ms must be an unsigned integer")?;
            if !(500..=30_000).contains(&window_ms) {
                bail!("fleet ble_window_ms must be between 500 and 30000");
            }
            self.ble
                .as_mut()
                .context("fleet ble_window_ms requires BLE collection to be enabled")?
                .window = Duration::from_millis(window_ms);
        }
        if object.contains_key("ble_zone") {
            self.ble
                .as_mut()
                .context("fleet ble_zone requires BLE collection")?
                .zone = string("ble_zone")?;
        }
        if object.contains_key("ble_movement_session") {
            self.ble
                .as_mut()
                .context("fleet ble_movement_session requires BLE collection")?
                .movement_session = string("ble_movement_session")?;
        }
        if object.contains_key("ble_sensor_moving") {
            let moving = object
                .get("ble_sensor_moving")
                .and_then(serde_json::Value::as_bool)
                .context("fleet ble_sensor_moving must be a boolean")?;
            self.ble
                .as_mut()
                .context("fleet ble_sensor_moving requires BLE collection")?
                .sensor_is_moving = moving;
        }
        Ok(())
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

fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match env_nonempty(name).as_deref() {
        Some("1" | "true" | "yes") => Ok(true),
        Some("0" | "false" | "no") => Ok(false),
        Some(value) => bail!("{name} must be true/false, yes/no, or 1/0; got {value}"),
        None => Ok(default),
    }
}

fn native_device_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Some(value) = nonempty_file("/etc/machine-id") {
        return Some(value);
    }
    #[cfg(target_os = "macos")]
    if let Some(value) = macos_host_uuid() {
        return Some(value);
    }
    env_nonempty("HOSTNAME").or_else(|| env_nonempty("COMPUTERNAME"))
}

fn native_boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    if let Some(value) = nonempty_file("/proc/sys/kernel/random/boot_id") {
        return Some(value);
    }
    #[cfg(target_os = "macos")]
    if let Some(value) = macos_boot_id() {
        return Some(value);
    }
    None
}

#[cfg(target_os = "linux")]
fn nonempty_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_spool_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/lib/radiochron-agent/spool")
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(target_os = "macos")]
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Library")
            .join("Application Support")
            .join("RadioChron")
            .join("spool");
        #[cfg(windows)]
        std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("radiochron-agent")
            .join("spool")
    }
}

#[cfg(target_os = "macos")]
fn macos_host_uuid() -> Option<String> {
    use core::ffi::c_void;

    #[link(name = "System")]
    extern "C" {
        fn gethostuuid(uuid: *mut u8, timeout: *const c_void) -> i32;
    }
    let mut uuid = [0u8; 16];
    if unsafe { gethostuuid(uuid.as_mut_ptr(), core::ptr::null()) } != 0 {
        return None;
    }
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        uuid[0], uuid[1], uuid[2], uuid[3], uuid[4], uuid[5], uuid[6], uuid[7],
        uuid[8], uuid[9], uuid[10], uuid[11], uuid[12], uuid[13], uuid[14], uuid[15]
    ))
}

#[cfg(target_os = "macos")]
fn macos_boot_id() -> Option<String> {
    use core::ffi::{c_char, c_void};

    #[repr(C)]
    struct Timeval {
        seconds: i64,
        microseconds: i32,
        padding: i32,
    }
    #[link(name = "System")]
    extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            old_len: *mut usize,
            new: *mut c_void,
            new_len: usize,
        ) -> i32;
    }
    let mut boot = Timeval {
        seconds: 0,
        microseconds: 0,
        padding: 0,
    };
    let mut size = std::mem::size_of::<Timeval>();
    let result = unsafe {
        sysctlbyname(
            c"kern.boottime".as_ptr(),
            (&mut boot as *mut Timeval).cast(),
            &mut size,
            core::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && boot.seconds > 0).then(|| format!("boot-{}", boot.seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            device_id: "device".into(),
            boot_id: "boot".into(),
            clock_quality: ClockQuality::Unknown,
            spool_dir: std::env::temp_dir(),
            spool_max_bytes: 1_024,
            poll_interval: Duration::from_secs(5),
            connectivity_interval: Duration::from_secs(30),
            connectivity: None,
            ble: None,
            mqtt_url: None,
            mqtt_topic: "radiochron/device/chronicle".into(),
            otlp_endpoint: None,
            prometheus_bind: None,
            tls: TlsConfig {
                ca_file: None,
                client_cert_file: None,
                client_key_file: None,
                server_name: None,
            },
            fleet_url: None,
            fleet_enroll_token: None,
            fleet_poll_interval: Duration::from_secs(60),
        }
    }

    #[test]
    fn signed_profile_can_enable_configure_and_disable_ble() {
        let mut config = config();
        config
            .apply_profile(&serde_json::json!({
                "ble_scan_seconds": 30,
                "ble_window_ms": 750,
                "ble_zone": "dock",
                "ble_sensor_moving": true
            }))
            .unwrap();
        let ble = config.ble.as_ref().unwrap();
        assert_eq!(ble.interval, Duration::from_secs(30));
        assert_eq!(ble.window, Duration::from_millis(750));
        assert_eq!(ble.zone.as_deref(), Some("dock"));
        assert!(ble.sensor_is_moving);

        config
            .apply_profile(&serde_json::json!({"ble_scan_seconds": 0}))
            .unwrap();
        assert!(config.ble.is_none());
    }
}
