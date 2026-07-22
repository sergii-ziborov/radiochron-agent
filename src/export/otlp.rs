use std::time::Duration;

use anyhow::bail;
use serde_json::{json, Value};

use super::Exporter;
use crate::http::HttpEndpoint;
use crate::transport::TlsConnector;

pub struct OtlpExporter {
    endpoint: HttpEndpoint,
    timeout: Duration,
    connector: TlsConnector,
}

impl OtlpExporter {
    pub fn new(url: &str, timeout: Duration, connector: TlsConnector) -> anyhow::Result<Self> {
        Ok(Self {
            endpoint: HttpEndpoint::parse(url, "/v1/logs")?,
            timeout,
            connector,
        })
    }
}

impl Exporter for OtlpExporter {
    fn name(&self) -> &'static str {
        "otlp_http_json"
    }

    fn export(&mut self, entry: &Value, raw: &[u8]) -> anyhow::Result<()> {
        let event_id = entry
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let device_id = entry
            .get("device_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let epoch = entry
            .get("epoch_seconds")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let body = serde_json::to_vec(&json!({
            "resourceLogs": [{
                "resource": {"attributes": [
                    {"key": "service.name", "value": {"stringValue": "radiochron-agent"}},
                    {"key": "device.id", "value": {"stringValue": device_id}}
                ]},
                "scopeLogs": [{
                    "scope": {"name": "radiochron-agent"},
                    "logRecords": [{
                        "timeUnixNano": epoch.saturating_mul(1_000_000_000).to_string(),
                        "severityText": "INFO",
                        "body": {"stringValue": String::from_utf8_lossy(raw)},
                        "attributes": [
                            {"key": "event.id", "value": {"stringValue": event_id}},
                            {"key": "radiochron.schema_version", "value": {"intValue": entry.get("schema_version").and_then(Value::as_u64).unwrap_or(0).to_string()}}
                        ]
                    }]
                }]
            }]
        }))?;
        let response = self.endpoint.request(
            "POST",
            &[("Content-Type", "application/json")],
            &body,
            self.timeout,
            &self.connector,
        )?;
        if !matches!(response.status, 200 | 202) {
            bail!("OTLP endpoint returned HTTP {}", response.status);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn tls() -> TlsConnector {
        TlsConnector::new(&crate::transport::TlsConfig::default()).unwrap()
    }

    #[test]
    fn parses_default_and_explicit_paths() {
        let default = HttpEndpoint::parse("http://127.0.0.1:4318", "/v1/logs").unwrap();
        assert_eq!(default.port, 4318);
        assert_eq!(default.path, "/v1/logs");
        let explicit = HttpEndpoint::parse("http://collector:80/custom", "/v1/logs").unwrap();
        assert_eq!(explicit.path, "/custom");
    }

    #[test]
    fn accepts_https_without_downgrading_the_scheme() {
        let endpoint = HttpEndpoint::parse("https://collector/v1/logs", "/v1/logs").unwrap();
        assert!(endpoint.tls);
        assert_eq!(endpoint.port, 443);
    }

    #[test]
    fn sends_otlp_json_and_accepts_http_200() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let read = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&request);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("POST /v1/logs HTTP/1.1"));
            assert!(request.contains("resourceLogs"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let mut exporter = OtlpExporter::new(
            &format!("http://{address}/v1/logs"),
            Duration::from_secs(1),
            tls(),
        )
        .unwrap();
        let entry = json!({
            "event_id":"device:boot:1",
            "device_id":"device",
            "epoch_seconds":1,
            "schema_version":1
        });
        exporter
            .export(&entry, br#"{"kind":"associated"}"#)
            .unwrap();
        server.join().unwrap();
    }
}
