# radiochron-agent

Lightweight daemon for the [RadioChron](https://github.com/sergii-ziborov/radiochron)
Wi-Fi diagnostics engine. It records locally first, survives exporter outages,
and runs on Linux/nl80211, Windows, or macOS/CoreWLAN without adding transport
dependencies to the core library.

This repository is intentionally separate from the core, MCP server, Node/npm
library, desktop app, fleet control plane, and website. The agent pins an exact
published RadioChron crate version and commits its Cargo.lock so stored schema
and collector behavior are reproducible.

`radiochron-agent` is the unattended IoT/fleet service. Interactive desktop use
lives in [`radiochron-electron`](https://github.com/sergii-ziborov/radiochron-electron),
which consumes the separate
[`radiochron-js`](https://github.com/sergii-ziborov/radiochron-js) Node library;
neither is an agent dependency.

## Data path

```text
WLAN API / nl80211 / CoreWLAN ----\
                                   -> generic Collector -> versioned chronicle entry
WinRT / BlueZ / CoreBluetooth ----/                         -> atomic disk spool
                                                             -> MQTT QoS 1 and/or OTLP/HTTP JSON
                                                             -> Prometheus /metrics (aggregate state)
```

Optional BLE collection uses the host Bluetooth stack through WinRT, BlueZ, or
CoreBluetooth. Raw advertisement addresses and payload bytes are processed in
memory but never written to the chronicle; persisted events contain only the
RadioChron identity, payload hash, RSSI, sensor ID, and evidence-based findings.

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
cargo install radiochron-agent

cargo build --release
sudo install -m 0755 target/release/radiochron-agent target/release/radiochron-agent-update /usr/local/bin/

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
| `RADIOCHRON_BOOT_ID` | Linux boot ID, macOS boot time, else process session | Override boot identity |
| `RADIOCHRON_CLOCK_QUALITY` | `unknown` | `synchronized`, `unsynchronized`, or `unknown` |
| `RADIOCHRON_SPOOL_DIR` | `/var/lib/radiochron-agent/spool` on Linux; user Application Support on macOS | Durable queue root |
| `RADIOCHRON_SPOOL_MAX_BYTES` | `67108864` | Event-file ceiling; at least one event is retained |
| `RADIOCHRON_POLL_SECONDS` | `5` | Native collector interval |
| `RADIOCHRON_MQTT_URL` | unset | `mqtt://` or `mqtts://` MQTT 3.1.1 QoS 1 endpoint |
| `RADIOCHRON_MQTT_TOPIC` | `radiochron/<device>/chronicle` | Event topic |
| `RADIOCHRON_OTLP_ENDPOINT` | unset | `http://` or `https://` OTLP Logs JSON endpoint |
| `RADIOCHRON_PROMETHEUS_BIND` | unset | Address for pull metrics, e.g. `127.0.0.1:9898` |
| `RADIOCHRON_TLS_CA_FILE` | system roots | Additional PEM CA bundle |
| `RADIOCHRON_TLS_CLIENT_CERT_FILE` | unset | PEM client certificate/chain for mTLS |
| `RADIOCHRON_TLS_CLIENT_KEY_FILE` | unset | Matching unencrypted PKCS#8 PEM key |
| `RADIOCHRON_TLS_SERVER_NAME` | endpoint host | Optional SNI/certificate-name override |

TLS is built in: SChannel on Windows, Secure Transport on Apple, and OpenSSL on
Linux. Server certificates and names are always validated. Supplying both
client files enables mTLS; supplying only one is a configuration error. There
is no insecure/skip-verification switch and schemes are never downgraded.

### Connectivity diagnosis

Set any of these to record the full network chain at
`RADIOCHRON_CONNECTIVITY_SECONDS` (default 30 seconds):

| Variable | Example | Layer |
|---|---|---|
| `RADIOCHRON_DNS_NAME` | `broker.lan` | DNS resolver |
| `RADIOCHRON_TCP_TARGET` | `broker.lan:1883` | LAN/application TCP |
| `RADIOCHRON_INTERNET_TARGET` | `health.example.net:443` | Explicit Internet reachability |
| `RADIOCHRON_CAPTIVE_PORTAL_URL` | `http://gateway.lan/generate_204` | Redirect/interception sentinel |
| `RADIOCHRON_TLS_TARGET` | `broker.lan:8883` | TLS certificate/name/handshake validation |
| `RADIOCHRON_QUALITY_TARGET` | `broker.lan:8883` | Repeated TCP loss and jitter sampling |
| `RADIOCHRON_QUALITY_ATTEMPTS` | `4` | Samples per quality diagnosis (1..20 in core) |
| `RADIOCHRON_CONNECTIVITY_TIMEOUT_MS` | `3000` | Per-target timeout |

### Bluetooth LE collection

BLE collection is opt-in so an unattended deployment never starts a radio scan
without explicit configuration:

| Variable | Default | Meaning |
|---|---:|---|
| `RADIOCHRON_BLE_SCAN_SECONDS` | `0` | Interval between scans; `0` disables BLE |
| `RADIOCHRON_BLE_WINDOW_MS` | `4000` | Bounded scan window, 500..30000 ms |
| `RADIOCHRON_BLE_ZONE` | unset | Caller-owned logical sensor zone |
| `RADIOCHRON_BLE_MOVEMENT_SESSION` | unset | Caller-owned movement segment for co-travel evidence |
| `RADIOCHRON_BLE_SENSOR_MOVING` | `false` | Whether the sensor is moving |

The same settings can be delivered in a signed fleet profile as
`ble_scan_seconds`, `ble_window_ms`, `ble_zone`,
`ble_movement_session`, and `ble_sensor_moving`. The scanner never connects to
unrelated peripherals or enumerates private GATT data. Linux requires BlueZ
and system D-Bus access; macOS app bundles require
`NSBluetoothAlwaysUsageDescription` and user Bluetooth permission.

Radio, authentication/association, exact IP assignment evidence, gateway, DNS,
TCP, captive portal, TLS certificate, packet-quality and Internet are reported
separately. Windows uses IP Helper's DHCP flag, macOS uses
SystemConfiguration, and Linux corroborates active lease/profile state. Linux
returns `unknown` when it cannot prove DHCP or static instead of guessing.

### Fleet enrollment, profiles, alarms and OTA

Point the agent at `radiochron-fleet` with:

| Variable | Default | Meaning |
|---|---:|---|
| `RADIOCHRON_FLEET_URL` | unset | Fleet base URL; HTTPS is mandatory outside loopback |
| `RADIOCHRON_FLEET_ENROLL_TOKEN` | unset | One-time/bootstrap enrollment token |
| `RADIOCHRON_FLEET_POLL_SECONDS` | `60` | Desired-state and heartbeat interval |

Enrollment returns a per-device token plus the fleet Ed25519 public key. Every
desired profile is verified before it can change collector/exporter settings.
The signed envelope also covers the OTA manifest; the artifact is downloaded
over HTTPS and checked against its SHA-256 digest. The separate updater swaps
the executable before service start and keeps the prior executable. If the new
agent does not write its health marker within the manifest timeout/retry
budget, the updater restores the previous binary. TLS trust material and fleet
credentials are deliberately not mutable through profiles.

On Unix systems the spool, per-device credential and fleet signing state are
created with owner-only directory/file permissions. Downloaded OTA artifacts
receive an owner execute bit before the atomic swap.

Prometheus exposes counters for recorded/exported/failed/dropped events, BLE
observations/findings, spool depth, and
`radiochron_connectivity_stage{layer=...}`. Stage values are `1`
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

## macOS background service

Recent macOS versions gate Wi-Fi SSID/BSSID and scan identity behind Location
Services. Apple does not grant that privacy permission to a system
LaunchDaemon, so full radio evidence must run as a per-user LaunchAgent. Build
an app bundle, request permission once in the logged-in session, then install
the supplied LaunchAgent:

```bash
sudo mkdir -p "/Applications/RadioChron Agent.app/Contents/MacOS"
sudo cp packaging/macos/Info.plist "/Applications/RadioChron Agent.app/Contents/Info.plist"
sudo install -m 0755 target/release/radiochron-agent target/release/radiochron-agent-update \
  "/Applications/RadioChron Agent.app/Contents/MacOS/"
sudo codesign --force --deep --sign - "/Applications/RadioChron Agent.app"
"/Applications/RadioChron Agent.app/Contents/MacOS/radiochron-agent" --request-location
mkdir -p ~/Library/LaunchAgents
cp packaging/io.radiochron.agent.user.plist ~/Library/LaunchAgents/io.radiochron.agent.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.radiochron.agent.plist
```

For a future signed distribution, replace the ad-hoc signature with a stable
Developer ID signature so the TCC identity survives upgrades. Edit the plist's
`EnvironmentVariables` dictionary to supply exporter/fleet settings before
bootstrap.

The system LaunchDaemon remains useful on a headless Mac for IP/gateway,
DNS/TCP/Internet, TLS, packet-quality and exporter health, but Apple may redact
SSID/BSSID and scan results:

Build both binaries on macOS, create the state/log directories, then install
the supplied Apple plist:

```bash
sudo mkdir -p "/Library/Application Support/RadioChron/spool/state/fleet" /Library/Logs/RadioChron
sudo install -m 0755 target/release/radiochron-agent target/release/radiochron-agent-update /usr/local/bin/
sudo install -m 0644 packaging/io.radiochron.agent.plist /Library/LaunchDaemons/
sudo launchctl bootstrap system /Library/LaunchDaemons/io.radiochron.agent.plist
```

Both service definitions run the updater as a small supervisor so replacement
and rollback happen while the agent executable is not open. CoreWLAN status
and scan use Apple's public `CWWiFiClient` API; no `airport` or
`system_profiler` shell-out is involved.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

MSRV is Rust 1.80. Release `0.3.0` tracks the published `radiochron` core
`0.5.0` and uses the Tokio-free `radiochron-native-ble` OS backend;
Cargo.lock fixes the complete daemon dependency graph.

Future crates.io releases are tag-driven. A `v<package-version>` tag runs the
full Linux release gate and publishes through the protected `crates-io`
GitHub environment. `CARGO_REGISTRY_TOKEN` belongs in that environment's
Actions secrets, never in source, workflow YAML or build artifacts.

## License

[MIT](LICENSE)
