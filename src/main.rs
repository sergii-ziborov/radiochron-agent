mod collector;
mod config;
mod export;
mod metrics;
mod spool;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use collector::AgentCollector;
use config::Config;
use export::{Exporter, MqttExporter, OtlpExporter};
use metrics::Metrics;
use radiochron::chronicle::{ChronicleIdentity, Recorder, RecorderOptions};
use spool::Spool;

fn main() -> anyhow::Result<()> {
    let argument = std::env::args().nth(1);
    if matches!(argument.as_deref(), Some("--help" | "-h")) {
        print_help();
        return Ok(());
    }
    if matches!(argument.as_deref(), Some("--version" | "-V")) {
        println!("radiochron-agent {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let config = Config::from_env()?;
    let mut exporters = build_exporters(&config)?;
    if argument.as_deref() == Some("--config-check") {
        println!(
            "configuration valid: device={} event_exporters={} connectivity={}",
            config.device_id,
            exporters.len(),
            config.connectivity.is_some()
        );
        return Ok(());
    }
    let metrics = Arc::new(Metrics::default());
    if let Some(bind) = &config.prometheus_bind {
        // Prometheus observes the same counters used by the spool and exporters.
        export::serve_prometheus(bind, Arc::clone(&metrics))?;
    }
    let spool = Spool::open(
        &config.spool_dir,
        config.spool_max_bytes,
        Arc::clone(&metrics),
    )
    .with_context(|| format!("open spool {}", config.spool_dir.display()))?;
    let initial_sequence = spool.next_sequence(&config.boot_id)?;
    let sink = spool.sink(&config.boot_id)?;
    let collector = AgentCollector::new(config.connectivity.clone(), config.connectivity_interval);
    let identity = ChronicleIdentity {
        device_id: Some(config.device_id.clone()),
        boot_id: config.boot_id.clone(),
        clock_quality: config.clock_quality,
    };
    let mut recorder = Recorder::with_collector(
        sink,
        collector,
        RecorderOptions {
            interval: config.poll_interval,
            identity,
            initial_sequence,
            ..RecorderOptions::default()
        },
    );

    let once = argument.as_deref() == Some("--once");
    if argument.is_some() && !once {
        anyhow::bail!("unknown argument; use --help");
    }
    eprintln!(
        "radiochron-agent: device={} spool={} exporters={}",
        config.device_id,
        config.spool_dir.display(),
        exporters.len()
    );
    let mut last_export_error = None;
    loop {
        let recorded = recorder.step()?;
        let drained = spool.drain(&mut exporters)?;
        if drained.last_error != last_export_error {
            if let Some(error) = &drained.last_error {
                eprintln!("radiochron-agent: export deferred: {error}");
            } else if last_export_error.is_some() {
                eprintln!("radiochron-agent: export recovered");
            }
            last_export_error = drained.last_error.clone();
        }
        if once {
            println!(
                "recorded={recorded} exported={} pending={}",
                drained.exported, drained.pending
            );
            return Ok(());
        }
        std::thread::sleep(config.poll_interval);
    }
}

fn build_exporters(config: &Config) -> anyhow::Result<Vec<Box<dyn Exporter>>> {
    let timeout = Duration::from_secs(5);
    let mut exporters: Vec<Box<dyn Exporter>> = Vec::new();
    if let Some(url) = &config.mqtt_url {
        exporters.push(Box::new(MqttExporter::new(
            url,
            config.mqtt_topic.clone(),
            &config.device_id,
            timeout,
        )?));
    }
    if let Some(url) = &config.otlp_endpoint {
        exporters.push(Box::new(OtlpExporter::new(url, timeout)?));
    }
    Ok(exporters)
}

fn print_help() {
    println!(
        "radiochron-agent — durable Wi-Fi chronicle daemon\n\n\
         Usage: radiochron-agent [--once|--config-check|--version]\n\n\
         Configure with RADIOCHRON_* environment variables; see README.md."
    );
}
