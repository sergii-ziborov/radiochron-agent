use std::time::{Duration, Instant};

use radiochron::chronicle::{
    Collector, CollectorEvent, CollectorSample, EntryKind, NativeCollector,
};
use radiochron::connectivity::ConnectivityConfig;

use crate::transport::TlsConnector;

pub struct AgentCollector {
    native: NativeCollector,
    connectivity: Option<ConnectivityConfig>,
    connectivity_interval: Duration,
    last_connectivity: Option<Instant>,
    tls: TlsConnector,
}

impl AgentCollector {
    pub fn new(
        connectivity: Option<ConnectivityConfig>,
        connectivity_interval: Duration,
        tls: TlsConnector,
    ) -> Self {
        Self {
            native: NativeCollector::default(),
            connectivity,
            connectivity_interval,
            last_connectivity: None,
            tls,
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
                        report: Box::new(radiochron::connectivity::diagnose_with_tls(
                            config,
                            |target, timeout| match self.tls.probe(target, timeout) {
                                Ok(probe) => radiochron::connectivity::DiagnosticStage {
                                    status: radiochron::connectivity::StageStatus::Pass,
                                    evidence: format!(
                                        "certificate verified; protocol={}, cipher={}, chain={}, leaf_sha256={}",
                                        probe.protocol,
                                        probe.cipher_suite,
                                        probe.certificate_chain_length,
                                        probe.leaf_sha256
                                    ),
                                    latency_ms: None,
                                },
                                Err(error) => radiochron::connectivity::DiagnosticStage {
                                    status: radiochron::connectivity::StageStatus::Fail,
                                    evidence: format!("TLS certificate/handshake failed: {error}"),
                                    latency_ms: None,
                                },
                            },
                        )),
                    },
                });
                self.last_connectivity = Some(Instant::now());
            }
        }
        Ok(events)
    }
}
