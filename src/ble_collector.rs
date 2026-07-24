use std::time::{Duration, Instant};

use radiochron::ble::{Observation, SensorContext, Tracker, TrackerPolicy};
use radiochron::chronicle::{CollectorEvent, EntryKind};

#[derive(Debug, Clone)]
pub struct BleOptions {
    pub interval: Duration,
    pub window: Duration,
    pub sensor_id: String,
    pub zone: Option<String>,
    pub movement_session: Option<String>,
    pub sensor_is_moving: bool,
}

pub struct BleCollector {
    options: BleOptions,
    tracker: Tracker,
    clock_origin: Instant,
    last_scan: Option<Instant>,
}

impl BleCollector {
    pub fn new(options: BleOptions) -> Self {
        Self {
            options,
            tracker: Tracker::new(TrackerPolicy::default()),
            clock_origin: Instant::now(),
            last_scan: None,
        }
    }

    pub fn collect_events(&mut self) -> anyhow::Result<Vec<CollectorEvent>> {
        let due = self
            .last_scan
            .map(|last| last.elapsed() >= self.options.interval)
            .unwrap_or(true);
        if !due {
            return Ok(Vec::new());
        }

        self.last_scan = Some(Instant::now());
        let scan = crate::ble_scan::scan(self.options.window)?;
        let monotonic_ms = self.clock_origin.elapsed().as_millis() as u64;
        let context = SensorContext {
            sensor_id: self.options.sensor_id.clone(),
            zone: self.options.zone.clone(),
            movement_session: self.options.movement_session.clone(),
            sensor_is_moving: self.options.sensor_is_moving,
        };
        let mut events = Vec::new();
        for advertisement in scan.advertisements {
            let rssi_dbm = advertisement.rssi_dbm;
            let result = self.tracker.observe(Observation {
                monotonic_ms,
                unix_epoch_ms: Some(scan.observed_at_epoch_ms as i64),
                context: context.clone(),
                advertisement,
            });
            events.push(CollectorEvent {
                interface_id: None,
                kind: EntryKind::BleObservation {
                    sensor_id: context.sensor_id.clone(),
                    identity: result.identity,
                    payload_hash: result.payload_hash,
                    rssi_dbm,
                },
            });
            events.extend(result.findings.into_iter().map(|finding| CollectorEvent {
                interface_id: None,
                kind: EntryKind::BleFinding { finding },
            }));
        }
        events.extend(scan.errors.into_iter().map(|message| CollectorEvent {
            interface_id: None,
            kind: EntryKind::CollectorError {
                source: "native_ble".into(),
                message,
            },
        }));
        events.extend(
            self.tracker
                .evaluate(monotonic_ms)
                .into_iter()
                .map(|finding| CollectorEvent {
                    interface_id: None,
                    kind: EntryKind::BleFinding { finding },
                }),
        );
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_timing_state_starts_due() {
        let collector = BleCollector::new(BleOptions {
            interval: Duration::from_secs(30),
            window: Duration::from_millis(500),
            sensor_id: "sensor".into(),
            zone: None,
            movement_session: None,
            sensor_is_moving: false,
        });
        assert!(collector.last_scan.is_none());
        assert_eq!(collector.tracker.histories().count(), 0);
    }
}
