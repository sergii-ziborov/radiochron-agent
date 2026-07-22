use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use native_tls::{Certificate, Identity, Protocol};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pub ca_file: Option<PathBuf>,
    pub client_cert_file: Option<PathBuf>,
    pub client_key_file: Option<PathBuf>,
    pub server_name: Option<String>,
}

pub trait IoStream: Read + Write + Send {}
impl<T: Read + Write + Send> IoStream for T {}
pub type BoxedIo = Box<dyn IoStream>;

#[derive(Clone)]
pub struct TlsConnector {
    connector: Arc<native_tls::TlsConnector>,
    server_name: Option<String>,
}

#[derive(Debug)]
pub struct TlsProbe {
    pub protocol: String,
    pub cipher_suite: String,
    pub certificate_chain_length: usize,
    pub leaf_sha256: String,
}

impl TlsConnector {
    pub fn new(options: &TlsConfig) -> anyhow::Result<Self> {
        if options.client_cert_file.is_some() != options.client_key_file.is_some() {
            bail!("TLS client certificate and private key must be configured together");
        }
        let mut builder = native_tls::TlsConnector::builder();
        builder.min_protocol_version(Some(Protocol::Tlsv12));
        if let Some(path) = &options.ca_file {
            let contents = std::fs::read(path)
                .with_context(|| format!("open TLS CA file {}", path.display()))?;
            let certificates = Certificate::stack_from_pem(&contents)
                .with_context(|| format!("parse TLS CA file {}", path.display()))?;
            if certificates.is_empty() {
                bail!("TLS CA file {} contains no certificates", path.display());
            }
            for certificate in certificates {
                builder.add_root_certificate(certificate);
            }
        }
        if let (Some(cert_path), Some(key_path)) =
            (&options.client_cert_file, &options.client_key_file)
        {
            let certificate = std::fs::read(cert_path)
                .with_context(|| format!("open TLS client certificate {}", cert_path.display()))?;
            let key = std::fs::read(key_path)
                .with_context(|| format!("open TLS client key {}", key_path.display()))?;
            let identity = Identity::from_pkcs8(&certificate, &key)
                .context("parse TLS client certificate/private key identity")?;
            builder.identity(identity);
        }
        Ok(Self {
            connector: Arc::new(builder.build().context("initialize platform TLS")?),
            server_name: options.server_name.clone(),
        })
    }

    pub fn connect(
        &self,
        host: &str,
        port: u16,
        tls: bool,
        timeout: Duration,
    ) -> anyhow::Result<BoxedIo> {
        let socket = configured_socket(host, port, timeout)?;
        if !tls {
            return Ok(Box::new(socket));
        }
        let server_name = self.server_name.as_deref().unwrap_or(host);
        let stream = self
            .connector
            .connect(server_name, socket)
            .map_err(|error| anyhow::anyhow!("TLS handshake for {server_name} failed: {error}"))?;
        Ok(Box::new(stream))
    }

    pub fn probe(&self, target: &str, timeout: Duration) -> anyhow::Result<TlsProbe> {
        let (host, port) = split_host_port(target, 443)?;
        let socket = configured_socket(&host, port, timeout)?;
        let server_name = self.server_name.as_deref().unwrap_or(&host);
        let stream = self
            .connector
            .connect(server_name, socket)
            .map_err(|error| anyhow::anyhow!("TLS handshake for {server_name} failed: {error}"))?;
        let leaf = stream
            .peer_certificate()?
            .context("TLS peer did not present a certificate")?
            .to_der()?;
        Ok(TlsProbe {
            protocol: "platform TLS (minimum TLS 1.2)".to_string(),
            cipher_suite: "platform-selected secure cipher".to_string(),
            certificate_chain_length: 1,
            leaf_sha256: hex(&Sha256::digest(&leaf)),
        })
    }
}

fn configured_socket(host: &str, port: u16, timeout: Duration) -> anyhow::Result<TcpStream> {
    let addresses = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolve {host}:{port}"))?;
    let mut last_error = None;
    let mut socket = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(connected) => {
                socket = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let socket = socket.ok_or_else(|| {
        anyhow::anyhow!(
            "connect to {host}:{port}: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "host resolved to no addresses".to_string())
        )
    })?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    Ok(socket)
}

pub fn split_host_port(authority: &str, default_port: u16) -> anyhow::Result<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .context("IPv6 authority is missing closing bracket")?;
        let port = suffix
            .strip_prefix(':')
            .map(|value| value.parse().context("invalid port"))
            .transpose()?
            .unwrap_or(default_port);
        return Ok((host.to_string(), port));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
        .map(|(host, port)| Ok::<_, anyhow::Error>((host.to_string(), port.parse()?)))
        .transpose()?
        .unwrap_or_else(|| (authority.to_string(), default_port));
    if host.is_empty() {
        bail!("host must not be empty");
    }
    Ok((host, port))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_parser_handles_dns_ipv4_and_ipv6() {
        assert_eq!(
            split_host_port("broker", 8883).unwrap(),
            ("broker".into(), 8883)
        );
        assert_eq!(split_host_port("127.0.0.1:9443", 443).unwrap().1, 9443);
        assert_eq!(
            split_host_port("[::1]:443", 80).unwrap(),
            ("::1".into(), 443)
        );
    }
}
