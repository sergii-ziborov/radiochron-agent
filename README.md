# radiochron-agent

Lightweight daemon for the [RadioChron](https://github.com/sergii-ziborov/radiochron)
Wi-Fi diagnostics engine. It records locally first, survives exporter outages,
and runs on Linux/nl80211 or Windows without adding transport dependencies to
the core library.

This repository is intentionally separate from the core, MCP server, npm
launcher and website. The agent pins an exact RadioChron commit so its stored
schema and collector behavior are reproducible.

## Data path

```text
WLAN API / nl80211 -> generic Collector -> versioned chronicle entry
                                      -> atomic disk spool
                                      -> MQTT QoS 1 and/or OTLP/HTTP JSON
                                      -> Prometheus /metrics (aggregate state)
```

The spool is the first write. An event is deleted only after every configured
event exporter acknowledges it. Delivery is therefore **at least once**: after
a partial exporter failure another exporter may receive the same entry again.
Consumers should deduplicate on `event_id`. When the size ceiling is reached,
the oldest entries are removed and `radiochron_spool_dropped_total` increases.

Every entry includes `schema_version`, `device_id`, `boot_id`, `sequence`,
clock quality and a deterministic `event_id`. Sequence recovery uses both an
append-only journal and pending files, so a restart does not reuse a sequence
that is still in the spool.

## Run

```bash
cargo build --release
sudo install -m 0755 target/release/radiochron-agent /usr/local/bin/

RADIOCHRON_DEVICE_ID=gateway-17 \
RADIOCHRON_SPOOL_DIR=./data/spool \
RADIOCHRON_PROMETHEUS_BIND=127.0.0.1:9898 \
cargo run -- --once
```

Without `--once`, the process stays in the foreground for systemd or another
service supervisor. SIGTERM may stop it immediately because every entry and
sequence update is synced before `Recorder::step` returns.

## Configuration

All settings are environment variables; none are sent anywhere unless an
exporter is explicitly configured.

| Variable | Default | Meaning |
|---|---:|---|
| `RADIOCHRON_DEVICE_ID` | `/etc/machine-id` or hostname | Stable fleet device identity |
| `RADIOCHRON_BOOT_ID` | Linux kernel boot ID, else process session | Override boot identity |
| `RADIOCHRON_CLOCK_QUALITY` | `unknown` | `synchronized`, `unsynchronized`, or `unknown` |
| `RADIOCHRON_SPOOL_DIR` | `/var/lib/radiochron-agent/spool` on Linux | Durable queue root |
| `RADIOCHRON_SPOOL_MAX_BYTES` | `67108864` | Event-file ceiling; at least one event is retained |
| `RADIOCHRON_POLL_SECONDS` | `5` | Native collector interval |
| `RADIOCHRON_MQTT_URL` | unset | `mqtt://host:port` enables MQTT 3.1.1 QoS 1 |
| `RADIOCHRON_MQTT_TOPIC` | `radiochron/<device>/chronicle` | Event topic |
| `RADIOCHRON_OTLP_ENDPOINT` | unset | `http://host:4318/v1/logs` enables OTLP Logs JSON |
| `RADIOCHRON_PROMETHEUS_BIND` | unset | Address for pull metrics, e.g. `127.0.0.1:9898` |

MQTT and OTLP intentionally accept plain local transports only. For production
TLS, put the agent behind a local authenticated broker/OTel Collector sidecar
or TLS proxy; `mqtts://` and `https://` are rejected rather than silently
downgraded. Credentials are therefore not embedded in URLs or spool files.

### Connectivity diagnosis

Set any of these to record the full network chain at
`RADIOCHRON_CONNECTIVITY_SECONDS` (default 30 seconds):

| Variable | Example | Layer |
|---|---|---|
| `RADIOCHRON_DNS_NAME` | `broker.lan` | DNS resolver |
| `RADIOCHRON_TCP_TARGET` | `broker.lan:1883` | LAN/application TCP |
| `RADIOCHRON_INTERNET_TARGET` | `health.example.net:443` | Explicit Internet reachability |
| `RADIOCHRON_CONNECTIVITY_TIMEOUT_MS` | `3000` | Per-target timeout |

Radio availability, AP authentication/association, IP configuration, DNS, TCP
and Internet are reported separately. The portable IP stage is named `dhcp`
for the operational layer, but its evidence explicitly states that it cannot
distinguish a DHCP lease from a static address without a platform-specific
lease database.

Prometheus exposes counters for recorded/exported/failed/dropped events, spool
depth, and `radiochron_connectivity_stage{layer=...}`. Stage values are `1`
pass, `0` fail, `-1` unknown and `-2` skipped. Prometheus is aggregate state;
it does not acknowledge or delete chronicle events.

## systemd autostart

```bash
sudo useradd --system --home /var/lib/radiochron-agent --shell /usr/sbin/nologin radiochron
sudo install -m 0644 packaging/radiochron-agent.service /etc/systemd/system/
sudo install -m 0600 /dev/null /etc/radiochron-agent.env
sudo systemctl daemon-reload
sudo systemctl enable --now radiochron-agent
```

Put environment assignments in `/etc/radiochron-agent.env`. The unit uses
`StateDirectory=radiochron-agent`, filesystem hardening, automatic restart and
only `CAP_NET_ADMIN`, which nl80211 drivers may require.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

MSRV is Rust 1.78. This repository has not been released yet.

## License

[MIT](LICENSE)
