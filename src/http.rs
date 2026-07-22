use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{bail, Context};

use crate::transport::{split_host_port, TlsConnector};

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HttpEndpoint {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub tls: bool,
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpEndpoint {
    pub fn parse(url: &str, default_path: &str) -> anyhow::Result<Self> {
        let (tls, rest, default_port) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest, 443)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest, 80)
        } else {
            bail!("HTTP endpoint must use http:// or https://");
        };
        let (authority, path) = rest
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((rest, default_path.to_string()));
        let (host, port) = split_host_port(authority, default_port)?;
        Ok(Self {
            host,
            port,
            path,
            tls,
        })
    }

    pub fn request(
        &self,
        method: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        timeout: Duration,
        connector: &TlsConnector,
    ) -> anyhow::Result<HttpResponse> {
        let mut stream = connector.connect(&self.host, self.port, self.tls, timeout)?;
        write!(
            stream,
            "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.path,
            self.host,
            self.port,
            body.len()
        )?;
        for (name, value) in headers {
            if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                bail!("HTTP header contains a line break");
            }
            write!(stream, "{name}: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(body)?;
        stream.flush()?;

        let mut raw = Vec::new();
        stream
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut raw)
            .context("read HTTP response")?;
        if raw.len() as u64 > MAX_RESPONSE_BYTES {
            bail!("HTTP response exceeds {MAX_RESPONSE_BYTES} bytes");
        }
        let header_end = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .context("HTTP response has no header terminator")?;
        let first_line = raw[..header_end]
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .context("invalid HTTP status line")?;
        let status = first_line
            .split_whitespace()
            .nth(1)
            .context("HTTP status code is missing")?
            .parse()
            .context("invalid HTTP status code")?;
        Ok(HttpResponse {
            status,
            body: raw[(header_end + 4)..].to_vec(),
        })
    }
}
