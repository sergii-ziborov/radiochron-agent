use std::time::{Duration, Instant};

use radiochron::chronicle::{
    Collector, CollectorEvent, CollectorSample, EntryKind, NativeCollector,
};
use radiochron::connectivity::ConnectivityConfig;

pub struct AgentCollector {
    native: NativeCollector,
    connectivity: Option<ConnectivityConfig>,
    connectivity_interval: Duration,
    last_connectivity: Option<Instant>,
}

impl AgentCollector {
    pub fn new(connectivity: Option<ConnectivityConfig>, connectivity_interval: Duration) -> Self {
        Self {
            native: NativeCollector::default(),
            connectivity,
            connectivity_interval,
            last_connectivity: None,
        }
    }
}

impl Collector for AgentCollector {
    fn name(&self) -> &'static str {
        self.native.name()
    }

    fn collect(&mut self) -> anyhow::Result<Vec<CollectorSample>> {
        self.native.collect()
    }

    fn collect_events(&mut self, interval: Duration) -> anyhow::Result<Vec<CollectorEvent>> {
        let mut events = self.native.collect_events(interval)?;
        let due = self
            .last_connectivity
            .map(|last| last.elapsed() >= self.connectivity_interval)
            .unwrap_or(true);
        if due {
            if let Some(config) = &self.connectivity {
                events.push(CollectorEvent {
                    interface_id: None,
                    kind: EntryKind::Connectivity {
                        report: Box::new(radiochron::connectivity::diagnose(config)),
                    },
                });
                self.last_connectivity = Some(Instant::now());
            }
        }
        Ok(events)
    }
}
