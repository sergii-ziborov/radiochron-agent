use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::Value;

use super::Exporter;
use crate::transport::{split_host_port, BoxedIo, TlsConnector};

pub struct MqttExporter {
    host: String,
    port: u16,
    topic: String,
    client_id: String,
    timeout: Duration,
    next_packet_id: u16,
    tls: bool,
    connector: TlsConnector,
    stream: Option<BoxedIo>,
}

impl MqttExporter {
    pub fn new(
        url: &str,
        topic: String,
        device_id: &str,
        timeout: Duration,
        connector: TlsConnector,
    ) -> anyhow::Result<Self> {
        let (tls, authority, default_port) = if let Some(authority) = url.strip_prefix("mqtts://") {
            (true, authority, 8883)
        } else if let Some(authority) = url.strip_prefix("mqtt://") {
            (false, authority, 1883)
        } else {
            bail!("MQTT URL must use mqtt:// or mqtts://");
        };
        let (host, port) = split_host_port(authority.trim_end_matches('/'), default_port)?;
        if host.is_empty() || topic.is_empty() {
            bail!("MQTT host and topic must not be empty");
        }
        Ok(Self {
            host,
            port,
            topic,
            client_id: format!("radiochron-{}", safe_id(device_id)),
            timeout,
            next_packet_id: 1,
            tls,
            connector,
            stream: None,
        })
    }

    fn connect(&self) -> anyhow::Result<BoxedIo> {
        let mut stream = self
            .connector
            .connect(&self.host, self.port, self.tls, self.timeout)?;
        let mut body = Vec::new();
        push_utf8(&mut body, "MQTT")?;
        body.extend([4, 2, 0, 30]); // MQTT 3.1.1, clean session, 30 s keepalive
        push_utf8(&mut body, &self.client_id)?;
        write_packet(&mut stream, 0x10, &body)?;
        let (kind, response) = read_packet(&mut stream)?;
        if kind != 0x20 || response.as_slice() != [0, 0] {
            bail!("MQTT broker rejected CONNECT: type={kind:#x} body={response:?}");
        }
        Ok(stream)
    }

    fn publish(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        if self.stream.is_none() {
            self.stream = Some(self.connect()?);
        }
        let packet_id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.wrapping_add(1).max(1);
        let mut body = Vec::with_capacity(self.topic.len() + payload.len() + 4);
        push_utf8(&mut body, &self.topic)?;
        body.extend(packet_id.to_be_bytes());
        body.extend(payload);

        let result = (|| {
            let stream = self.stream.as_mut().expect("connected above");
            write_packet(stream, 0x32, &body)?; // PUBLISH, QoS 1
            let (kind, response) = read_packet(stream)?;
            if kind != 0x40 || response.as_slice() != packet_id.to_be_bytes() {
                bail!("invalid MQTT PUBACK: type={kind:#x} body={response:?}");
            }
            Ok(())
        })();
        if result.is_err() {
            self.stream = None;
        }
        result
    }
}

impl Exporter for MqttExporter {
    fn name(&self) -> &'static str {
        "mqtt_qos1"
    }

    fn export(&mut self, _entry: &Value, raw: &[u8]) -> anyhow::Result<()> {
        self.publish(raw)
    }
}

fn push_utf8(output: &mut Vec<u8>, value: &str) -> anyhow::Result<()> {
    let len: u16 = value.len().try_into().context("MQTT string is too long")?;
    output.extend(len.to_be_bytes());
    output.extend(value.as_bytes());
    Ok(())
}

fn write_packet<S: Write + ?Sized>(stream: &mut S, header: u8, body: &[u8]) -> anyhow::Result<()> {
    stream.write_all(&[header])?;
    let mut remaining = body.len();
    loop {
        let mut byte = (remaining % 128) as u8;
        remaining /= 128;
        if remaining > 0 {
            byte |= 0x80;
        }
        stream.write_all(&[byte])?;
        if remaining == 0 {
            break;
        }
    }
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn read_packet<S: Read + ?Sized>(stream: &mut S) -> anyhow::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 1];
    stream.read_exact(&mut header)?;
    let mut multiplier = 1usize;
    let mut remaining = 0usize;
    for _ in 0..4 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte)?;
        remaining += ((byte[0] & 0x7f) as usize) * multiplier;
        if byte[0] & 0x80 == 0 {
            let mut body = vec![0; remaining];
            stream.read_exact(&mut body)?;
            return Ok((header[0] & 0xf0, body));
        }
        multiplier *= 128;
    }
    bail!("invalid MQTT remaining length")
}

fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tls() -> TlsConnector {
        TlsConnector::new(&crate::transport::TlsConfig::default()).unwrap()
    }

    #[test]
    fn parses_default_port_and_sanitizes_client_id() {
        let exporter = MqttExporter::new(
            "mqtt://broker.lan",
            "radiochron/events".into(),
            "lab/device 1",
            Duration::from_secs(1),
            tls(),
        )
        .unwrap();
        assert_eq!(exporter.port, 1883);
        assert_eq!(exporter.client_id, "radiochron-lab_device_1");
    }

    #[test]
    fn accepts_tls_scheme_on_the_standard_port() {
        let exporter = MqttExporter::new(
            "mqtts://broker.lan",
            "events".into(),
            "device",
            Duration::from_secs(1),
            tls(),
        )
        .unwrap();
        assert!(exporter.tls);
        assert_eq!(exporter.port, 8883);
    }

    #[test]
    fn completes_connect_publish_and_puback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (kind, _) = read_packet(&mut stream).unwrap();
            assert_eq!(kind, 0x10);
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
            let (kind, publish) = read_packet(&mut stream).unwrap();
            assert_eq!(kind, 0x30);
            let topic_len = u16::from_be_bytes([publish[0], publish[1]]) as usize;
            let packet_offset = 2 + topic_len;
            let packet_id = [publish[packet_offset], publish[packet_offset + 1]];
            stream
                .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                .unwrap();
        });
        let mut exporter = MqttExporter::new(
            &format!("mqtt://{address}"),
            "radiochron/events".into(),
            "device",
            Duration::from_secs(1),
            tls(),
        )
        .unwrap();
        exporter
            .export(&Value::Null, br#"{"event_id":"one"}"#)
            .unwrap();
        broker.join().unwrap();
    }
}
