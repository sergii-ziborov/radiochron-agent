use std::time::Duration;

use radiochron::ble::Advertisement;

pub struct NativeScan {
    pub observed_at_epoch_ms: i64,
    pub advertisements: Vec<Advertisement>,
    pub errors: Vec<String>,
}

pub fn scan(duration: Duration) -> anyhow::Result<NativeScan> {
    let report = radiochron_native_ble::scan(duration).map_err(annotate_platform_error)?;
    Ok(NativeScan {
        observed_at_epoch_ms: report.observed_at_epoch_ms,
        advertisements: report.advertisements,
        errors: report.errors,
    })
}

fn annotate_platform_error(error: radiochron_native_ble::Error) -> anyhow::Error {
    #[cfg(target_vendor = "apple")]
    return anyhow::anyhow!(
        "native BLE scan failed: {error}. Grant Bluetooth permission; app bundles need NSBluetoothAlwaysUsageDescription"
    );
    #[cfg(target_os = "linux")]
    return anyhow::anyhow!(
        "native BLE scan failed: {error}. Ensure BlueZ is running and the service can access the system D-Bus"
    );
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    anyhow::anyhow!("native BLE scan failed: {error}")
}
