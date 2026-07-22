use std::fmt::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use radiochron::chronicle::{Entry, EntryKind};
use radiochron::connectivity::StageStatus;

#[derive(Debug)]
pub struct Metrics {
    events_recorded: AtomicU64,
    events_exported: AtomicU64,
    export_failures: AtomicU64,
    spool_dropped: AtomicU64,
    spool_depth: AtomicU64,
    radio: AtomicI64,
    authentication: AtomicI64,
    dhcp: AtomicI64,
    dns: AtomicI64,
    tcp: AtomicI64,
    internet: AtomicI64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            events_recorded: AtomicU64::new(0),
            events_exported: AtomicU64::new(0),
            export_failures: AtomicU64::new(0),
            spool_dropped: AtomicU64::new(0),
            spool_depth: AtomicU64::new(0),
            radio: AtomicI64::new(-1),
            authentication: AtomicI64::new(-1),
            dhcp: AtomicI64::new(-1),
            dns: AtomicI64::new(-1),
            tcp: AtomicI64::new(-1),
            internet: AtomicI64::new(-1),
        }
    }
}

impl Metrics {
    pub fn record_entry(&self, entry: &Entry) {
        self.events_recorded.fetch_add(1, Ordering::Relaxed);
        if let EntryKind::Connectivity { report } = &entry.kind {
            self.radio
                .store(code(report.radio.status), Ordering::Relaxed);
            self.authentication
                .store(code(report.authentication.status), Ordering::Relaxed);
            self.dhcp.store(code(report.dhcp.status), Ordering::Relaxed);
            self.dns.store(code(report.dns.status), Ordering::Relaxed);
            self.tcp.store(code(report.tcp.status), Ordering::Relaxed);
            self.internet
                .store(code(report.internet.status), Ordering::Relaxed);
        }
    }

    pub fn exported(&self) {
        self.events_exported.fetch_add(1, Ordering::Relaxed);
    }

    pub fn export_failed(&self) {
        self.export_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dropped(&self) {
        self.spool_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_spool_depth(&self, depth: usize) {
        self.spool_depth.store(depth as u64, Ordering::Relaxed);
    }

    pub fn render(&self) -> String {
        let mut out = String::from(
            "# HELP radiochron_agent_up Whether the agent metrics endpoint is running.\n\
             # TYPE radiochron_agent_up gauge\n\
             radiochron_agent_up 1\n",
        );
        counter(
            &mut out,
            "radiochron_events_recorded_total",
            self.events_recorded.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "radiochron_events_exported_total",
            self.events_exported.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "radiochron_export_failures_total",
            self.export_failures.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "radiochron_spool_dropped_total",
            self.spool_dropped.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "radiochron_spool_depth",
            self.spool_depth.load(Ordering::Relaxed) as i64,
        );
        for (layer, value) in [
            ("radio", self.radio.load(Ordering::Relaxed)),
            (
                "authentication",
                self.authentication.load(Ordering::Relaxed),
            ),
            ("dhcp", self.dhcp.load(Ordering::Relaxed)),
            ("dns", self.dns.load(Ordering::Relaxed)),
            ("tcp", self.tcp.load(Ordering::Relaxed)),
            ("internet", self.internet.load(Ordering::Relaxed)),
        ] {
            let _ = writeln!(
                out,
                "radiochron_connectivity_stage{{layer=\"{layer}\"}} {value}"
            );
        }
        out
    }
}

fn code(status: StageStatus) -> i64 {
    match status {
        StageStatus::Pass => 1,
        StageStatus::Fail => 0,
        StageStatus::Unknown => -1,
        StageStatus::Skipped => -2,
    }
}

fn counter(output: &mut String, name: &str, value: u64) {
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name} {value}");
}

fn gauge(output: &mut String, name: &str, value: i64) {
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        let metrics = Metrics::default();
        metrics.exported();
        metrics.set_spool_depth(3);
        let text = metrics.render();
        assert!(text.contains("radiochron_events_exported_total 1"));
        assert!(text.contains("radiochron_spool_depth 3"));
    }
}
