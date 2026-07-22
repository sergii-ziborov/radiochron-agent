use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use crate::metrics::Metrics;

pub fn serve_prometheus(bind: &str, metrics: Arc<Metrics>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind)?;
    let bind = listener.local_addr()?;
    std::thread::Builder::new()
        .name("radiochron-prometheus".into())
        .spawn(move || {
            eprintln!("radiochron-agent: Prometheus listening on http://{bind}/metrics");
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut request = [0u8; 1024];
                let read = stream.read(&mut request).unwrap_or(0);
                let is_metrics = request[..read].starts_with(b"GET /metrics ");
                let (status, body, content_type) = if is_metrics {
                    ("200 OK", metrics.render(), "text/plain; version=0.0.4")
                } else {
                    ("404 Not Found", "not found\n".to_string(), "text/plain")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        })?;
    Ok(())
}
