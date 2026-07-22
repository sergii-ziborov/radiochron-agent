use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use radiochron::chronicle::{Entry, Sink};
use serde_json::Value;

use crate::export::Exporter;
use crate::metrics::Metrics;

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Spool {
    events_dir: PathBuf,
    state_dir: PathBuf,
    max_bytes: u64,
    metrics: Arc<Metrics>,
}

pub struct SpoolSink {
    spool: Spool,
    sequence: File,
}

#[derive(Debug, Default)]
pub struct DrainReport {
    pub exported: usize,
    pub pending: usize,
    pub last_error: Option<String>,
}

impl Spool {
    pub fn open(root: impl AsRef<Path>, max_bytes: u64, metrics: Arc<Metrics>) -> io::Result<Self> {
        let root = root.as_ref();
        let events_dir = root.join("events");
        let state_dir = root.join("state");
        std::fs::create_dir_all(&events_dir)?;
        std::fs::create_dir_all(&state_dir)?;
        crate::private_fs::restrict_directory(root)?;
        crate::private_fs::restrict_directory(&events_dir)?;
        crate::private_fs::restrict_directory(&state_dir)?;
        let spool = Self {
            events_dir,
            state_dir,
            max_bytes,
            metrics,
        };
        spool.update_depth()?;
        Ok(spool)
    }

    pub fn sink(&self, boot_id: &str) -> io::Result<SpoolSink> {
        let sequence_path = self.sequence_path(boot_id);
        let sequence = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sequence_path)?;
        crate::private_fs::restrict_file(&sequence_path)?;
        Ok(SpoolSink {
            spool: self.clone(),
            sequence,
        })
    }

    pub fn next_sequence(&self, boot_id: &str) -> io::Result<u64> {
        let mut highest = std::fs::read_to_string(self.sequence_path(boot_id))
            .ok()
            .into_iter()
            .flat_map(|text| {
                text.lines()
                    .filter_map(|line| line.parse::<u64>().ok())
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0);
        for path in self.pending_paths()? {
            let Ok(raw) = std::fs::read(&path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            if entry.get("boot_id").and_then(Value::as_str) == Some(boot_id) {
                highest = highest.max(entry.get("sequence").and_then(Value::as_u64).unwrap_or(0));
            }
        }
        Ok(highest.saturating_add(1).max(1))
    }

    pub fn drain(&self, exporters: &mut [Box<dyn Exporter>]) -> io::Result<DrainReport> {
        let paths = self.pending_paths()?;
        if exporters.is_empty() {
            self.metrics.set_spool_depth(paths.len());
            return Ok(DrainReport {
                pending: paths.len(),
                ..DrainReport::default()
            });
        }

        let mut report = DrainReport::default();
        for path in paths {
            let raw = std::fs::read(&path)?;
            let entry = match serde_json::from_slice::<Value>(&raw) {
                Ok(entry) => entry,
                Err(error) => {
                    let quarantined = path.with_extension("invalid");
                    std::fs::rename(&path, quarantined)?;
                    self.metrics.dropped();
                    report.last_error = Some(format!(
                        "quarantined invalid spool entry {}: {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            let mut delivered = true;
            for exporter in exporters.iter_mut() {
                if let Err(error) = exporter.export(&entry, &raw) {
                    delivered = false;
                    self.metrics.export_failed();
                    report.last_error = Some(format!("{}: {error}", exporter.name()));
                    break;
                }
            }
            if !delivered {
                break; // preserve event order; already delivered copies dedupe by event_id
            }
            std::fs::remove_file(path)?;
            self.metrics.exported();
            report.exported += 1;
        }
        report.pending = self.update_depth()?;
        Ok(report)
    }

    fn store(&self, entry: &Entry) -> io::Result<()> {
        let raw = serde_json::to_vec(entry).map_err(io::Error::other)?;
        let name = format!(
            "{:020}-{}-{:020}.json",
            entry.epoch_seconds.max(0),
            safe_component(&entry.boot_id),
            entry.sequence
        );
        let target = self.events_dir.join(name);
        if target.exists() {
            return Ok(()); // same boot+sequence is the same logical event
        }
        let temp = self.events_dir.join(format!(
            ".{}-{}-{}.tmp",
            std::process::id(),
            entry.sequence,
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        crate::private_fs::restrict_file(&temp)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)?;
        self.metrics.record_entry(entry);
        self.enforce_limit()?;
        Ok(())
    }

    fn enforce_limit(&self) -> io::Result<()> {
        let paths = self.pending_paths()?;
        let mut sized = paths
            .into_iter()
            .map(|path| {
                let size = path.metadata()?.len();
                Ok((path, size))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut total: u64 = sized.iter().map(|(_, size)| size).sum();
        while total > self.max_bytes && sized.len() > 1 {
            let (oldest, size) = sized.remove(0);
            std::fs::remove_file(oldest)?;
            total = total.saturating_sub(size);
            self.metrics.dropped();
        }
        self.metrics.set_spool_depth(sized.len());
        Ok(())
    }

    fn pending_paths(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths = std::fs::read_dir(&self.events_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    fn update_depth(&self) -> io::Result<usize> {
        let depth = self.pending_paths()?.len();
        self.metrics.set_spool_depth(depth);
        Ok(depth)
    }

    fn sequence_path(&self, boot_id: &str) -> PathBuf {
        self.state_dir
            .join(format!("sequence-{}.journal", safe_component(boot_id)))
    }
}

impl Sink for SpoolSink {
    fn write(&mut self, entry: &Entry) -> io::Result<()> {
        self.spool.store(entry)?;
        writeln!(self.sequence, "{}", entry.sequence)?;
        self.sequence.sync_data()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sequence.flush()
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use radiochron::chronicle::{ChronicleIdentity, ClockQuality, EntryKind};

    #[test]
    fn resumes_sequence_from_durable_spool() {
        let root = std::env::temp_dir().join(format!("radiochron-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let spool = Spool::open(&root, 1024 * 1024, Arc::new(Metrics::default())).unwrap();
        let mut sink = spool.sink("boot-a").unwrap();
        let entry = Entry::stamped(
            &ChronicleIdentity {
                device_id: Some("device".into()),
                boot_id: "boot-a".into(),
                clock_quality: ClockQuality::Synchronized,
            },
            7,
            None,
            EntryKind::CollectorRecovered {
                source: "test".into(),
            },
        );
        sink.write(&entry).unwrap();
        assert_eq!(spool.next_sequence("boot-a").unwrap(), 8);
        assert_eq!(spool.pending_paths().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
