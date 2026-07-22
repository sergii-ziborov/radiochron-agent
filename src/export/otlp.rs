use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use super::Exporter;

pub struct OtlpExporter {
    endpoint: HttpEndpoint,
    timeout: Duration,
}

impl OtlpExporter {
    pub fn new(url: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            endpoint: HttpEndpoint::parse(url)?,
            timeout,
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
        self.endpoint.post(&body, self.timeout)
    }
}

struct HttpEndpoint {
    host: String,
    port: u16,
    path: String,
}

impl HttpEndpoint {
    fn parse(url: &str) -> anyhow::Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .context("OTLP endpoint must use http:// (use a local TLS proxy for HTTPS)")?;
        let (authority, path) = rest
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((rest, "/v1/logs".to_string()));
        let (host, port) = authority
            .rsplit_once(':')
            .map(|(host, port)| {
                Ok::<_, anyhow::Error>((
                    host.to_string(),
                    port.parse().context("invalid OTLP port")?,
                ))
            })
            .transpose()?
            .unwrap_or_else(|| (authority.to_string(), 80));
        if host.is_empty() {
            bail!("OTLP endpoint host is empty");
        }
        Ok(Self { host, port, path })
    }

    fn post(&self, body: &[u8], timeout: Duration) -> anyhow::Result<()> {
        let mut stream = TcpStream::connect((&*self.host, self.port))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        write!(
            stream,
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            self.port,
            body.len()
        )?;
        stream.write_all(body)?;
        stream.flush()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        let status = response
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .unwrap_or("invalid response");
        if !(status.contains(" 200 ") || status.contains(" 202 ")) {
            bail!("OTLP endpoint returned {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_explicit_paths() {
        let default = HttpEndpoint::parse("http://127.0.0.1:4318").unwrap();
        assert_eq!(default.port, 4318);
        assert_eq!(default.path, "/v1/logs");
        let explicit = HttpEndpoint::parse("http://collector:80/custom").unwrap();
        assert_eq!(explicit.path, "/custom");
    }

    #[test]
    fn rejects_https_until_a_tls_backend_is_configured() {
        assert!(HttpEndpoint::parse("https://collector/v1/logs").is_err());
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
        let mut exporter =
            OtlpExporter::new(&format!("http://{address}/v1/logs"), Duration::from_secs(1))
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
