mod mqtt;
mod otlp;
mod prometheus;

use serde_json::Value;

pub use mqtt::MqttExporter;
pub use otlp::OtlpExporter;
pub use prometheus::serve_prometheus;

pub trait Exporter {
    fn name(&self) -> &'static str;
    fn export(&mut self, entry: &Value, raw: &[u8]) -> anyhow::Result<()>;
}
