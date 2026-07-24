use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use btleplug::api::{
    AddressType as NativeAddressType, Central, CentralEvent, Manager as _, Peripheral, ScanFilter,
};
use btleplug::platform::Manager;
use futures::StreamExt;
use radiochron::ble::{AddressType, Advertisement, ManufacturerData, ServiceData};

pub struct NativeScan {
    pub observed_at_epoch_ms: u64,
    pub advertisements: Vec<Advertisement>,
    pub errors: Vec<String>,
}

pub fn scan(duration: Duration) -> anyhow::Result<NativeScan> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime
        .block_on(scan_async(duration))
        .map_err(annotate_platform_error)
}

async fn scan_async(duration: Duration) -> anyhow::Result<NativeScan> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    if adapters.is_empty() {
        anyhow::bail!("no Bluetooth adapters found");
    }

    let mut started = Vec::with_capacity(adapters.len());
    let mut events = futures::stream::SelectAll::new();
    let mut errors = Vec::new();
    for (index, adapter) in adapters.iter().enumerate() {
        match (
            adapter.events().await,
            adapter.start_scan(ScanFilter::default()).await,
        ) {
            (Ok(stream), Ok(())) => {
                started.push(true);
                events.push(stream.map(move |event| (index, event)).boxed());
            }
            (Err(error), _) => {
                started.push(false);
                errors.push(format!("adapter {index} event stream: {error}"));
            }
            (_, Err(error)) => {
                started.push(false);
                errors.push(format!("adapter {index} start scan: {error}"));
            }
        }
    }
    if !started.iter().any(|value| *value) {
        anyhow::bail!("Bluetooth scan could not start: {}", errors.join("; "));
    }

    let started_at = Instant::now();
    let mut observed = (0..adapters.len())
        .map(|_| std::collections::BTreeSet::new())
        .collect::<Vec<_>>();
    while started_at.elapsed() < duration {
        let remaining = duration.saturating_sub(started_at.elapsed());
        tokio::select! {
            event = events.next() => {
                if let Some((adapter_index, event)) = event {
                    let id = match event {
                        CentralEvent::DeviceDiscovered(id)
                        | CentralEvent::DeviceUpdated(id)
                        | CentralEvent::ManufacturerDataAdvertisement { id, .. }
                        | CentralEvent::ServiceDataAdvertisement { id, .. }
                        | CentralEvent::ServicesAdvertisement { id, .. } => Some(id),
                        _ => None,
                    };
                    if let Some(id) = id {
                        observed[adapter_index].insert(id);
                    }
                }
            }
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(250))) => {}
        }
    }

    let mut advertisements = Vec::new();
    for (index, adapter) in adapters.iter().enumerate() {
        if !started[index] {
            continue;
        }
        if let Err(error) = adapter.stop_scan().await {
            errors.push(format!("adapter {index} stop scan: {error}"));
        }
        for id in &observed[index] {
            let peripheral = match adapter.peripheral(id).await {
                Ok(peripheral) => peripheral,
                Err(error) => {
                    errors.push(format!("adapter {index} resolve peripheral: {error}"));
                    continue;
                }
            };
            match peripheral.properties().await {
                Ok(Some(properties)) => {
                    if let Some(rssi_dbm) = properties.rssi {
                        advertisements.push(to_advertisement(
                            peripheral.id().to_string(),
                            properties,
                            rssi_dbm,
                        ));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    errors.push(format!("adapter {index} read peripheral: {error}"));
                }
            }
        }
    }

    Ok(NativeScan {
        observed_at_epoch_ms: epoch_millis(),
        advertisements,
        errors,
    })
}

fn to_advertisement(
    peripheral_id: String,
    properties: btleplug::api::PeripheralProperties,
    rssi_dbm: i16,
) -> Advertisement {
    let address_type = match properties.address_type {
        Some(NativeAddressType::Public) => AddressType::Public,
        Some(NativeAddressType::Random) | None => AddressType::Unknown,
    };
    #[cfg(target_vendor = "apple")]
    let address = peripheral_id;
    #[cfg(not(target_vendor = "apple"))]
    let address = properties.address.to_string();
    #[cfg(not(target_vendor = "apple"))]
    let _ = peripheral_id;

    let mut manufacturer_data = properties
        .manufacturer_data
        .into_iter()
        .map(|(company_id, data)| ManufacturerData { company_id, data })
        .collect::<Vec<_>>();
    manufacturer_data.sort_by_key(|item| item.company_id);
    let mut service_data = properties
        .service_data
        .into_iter()
        .map(|(uuid, data)| ServiceData {
            uuid: uuid.to_string(),
            data,
        })
        .collect::<Vec<_>>();
    service_data.sort_by(|left, right| left.uuid.cmp(&right.uuid));
    let mut service_uuids = properties
        .services
        .into_iter()
        .map(|uuid| uuid.to_string())
        .collect::<Vec<_>>();
    service_uuids.sort();

    Advertisement {
        address,
        address_type,
        local_name: properties.local_name,
        rssi_dbm,
        tx_power_dbm: properties.tx_power_level,
        connectable: None,
        service_uuids,
        manufacturer_data,
        service_data,
        protocol_identity: None,
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn annotate_platform_error(error: anyhow::Error) -> anyhow::Error {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn native_properties_are_normalized() {
        let mut properties = btleplug::api::PeripheralProperties {
            address: "01:02:03:04:05:06".parse().unwrap(),
            address_type: Some(NativeAddressType::Public),
            local_name: Some("sensor".into()),
            rssi: Some(-42),
            ..Default::default()
        };
        properties.manufacturer_data = HashMap::from([(2, vec![2]), (1, vec![1])]);
        let advertisement = to_advertisement("native-id".into(), properties, -42);
        assert_eq!(advertisement.manufacturer_data[0].company_id, 1);
        assert_eq!(advertisement.address_type, AddressType::Public);
    }
}
