mod ble_collector;
mod ble_scan;
mod collector;
mod config;
mod export;
mod fleet;
mod http;
#[cfg(target_os = "macos")]
mod macos_location;
mod metrics;
mod ota;
mod private_fs;
mod spool;
mod transport;
mod update_fs;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use collector::AgentCollector;
use config::Config;
use export::{Exporter, MqttExporter, OtlpExporter};
use fleet::FleetClient;
use metrics::Metrics;
use radiochron::chronicle::{ChronicleIdentity, Recorder, RecorderOptions};
use spool::Spool;
use transport::TlsConnector;

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
    if matches!(argument.as_deref(), Some("--request-location")) {
        #[cfg(target_os = "macos")]
        return macos_location::request_authorization();
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("--request-location is available only on macOS");
    }

    let mut config = Config::from_env()?;
    let tls = TlsConnector::new(&config.tls)?;
    let config_check = argument.as_deref() == Some("--config-check");
    let fleet_sync = argument.as_deref() == Some("--fleet-sync");
    let mut fleet = FleetClient::open(&config, tls.clone())?;
    if !config_check {
        if let Some(client) = fleet.as_mut() {
            client.apply_cached(&mut config)?;
            if let Err(error) = client.bootstrap(&mut config) {
                if fleet_sync {
                    return Err(error);
                }
                eprintln!("radiochron-agent: fleet bootstrap deferred: {error}");
            }
        }
    }
    if fleet_sync {
        if fleet.is_none() {
            anyhow::bail!("--fleet-sync requires RADIOCHRON_FLEET_URL");
        }
        println!("fleet synchronization complete for {}", config.device_id);
        return Ok(());
    }
    let mut exporters = build_exporters(&config, &tls)?;
    if config_check {
        println!(
            "configuration valid: device={} event_exporters={} connectivity={} ble={}",
            config.device_id,
            exporters.len(),
            config.connectivity.is_some(),
            config.ble.is_some()
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
    let collector = AgentCollector::new(
        config.connectivity.clone(),
        config.connectivity_interval,
        config.ble.clone(),
        tls.clone(),
    );
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
        ota::mark_healthy(&config.spool_dir)?;
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
        if let Some(client) = fleet.as_mut() {
            match client.tick(&mut config, &metrics) {
                Ok(true) => anyhow::bail!(
                    "fleet profile changed; exiting so the service manager restarts with the new configuration"
                ),
                Ok(false) => {}
                Err(error) => eprintln!("radiochron-agent: fleet sync deferred: {error}"),
            }
        }
        std::thread::sleep(config.poll_interval);
    }
}

fn build_exporters(config: &Config, tls: &TlsConnector) -> anyhow::Result<Vec<Box<dyn Exporter>>> {
    let timeout = Duration::from_secs(5);
    let mut exporters: Vec<Box<dyn Exporter>> = Vec::new();
    if let Some(url) = &config.mqtt_url {
        exporters.push(Box::new(MqttExporter::new(
            url,
            config.mqtt_topic.clone(),
            &config.device_id,
            timeout,
            tls.clone(),
        )?));
    }
    if let Some(url) = &config.otlp_endpoint {
        exporters.push(Box::new(OtlpExporter::new(url, timeout, tls.clone())?));
    }
    Ok(exporters)
}

fn print_help() {
    println!(
        "radiochron-agent - durable Wi-Fi and BLE chronicle daemon\n\n\
         Usage: radiochron-agent [--once|--fleet-sync|--config-check|--request-location|--version]\n\n\
         Configure with RADIOCHRON_* environment variables; see README.md."
    );
}
