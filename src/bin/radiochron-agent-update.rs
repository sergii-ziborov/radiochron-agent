//! Small pre-start updater. It is intentionally a separate process so it can
//! replace the agent executable on Windows as well as Linux/macOS.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};

#[path = "../private_fs.rs"]
#[allow(dead_code)]
mod private_fs;
#[path = "../update_fs.rs"]
mod update_fs;

use update_fs::{atomic_json, digest_file, replace_file};

#[derive(Debug, Serialize, Deserialize)]
struct PendingUpdate {
    schema_version: u32,
    device_id: String,
    profile_revision: u64,
    version: String,
    artifact_path: PathBuf,
    target_path: PathBuf,
    sha256: String,
    health_timeout_seconds: u64,
    phase: String,
    attempts: u8,
    #[serde(default)]
    applied_at_epoch_seconds: i64,
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "--help".to_string());
    if command == "--help" || command == "-h" {
        println!(
            "radiochron-agent-update reconcile --state-dir DIR\n\
             radiochron-agent-update supervise --state-dir DIR --agent PATH"
        );
        return Ok(());
    }
    let mut state_dir = None;
    let mut agent = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--state-dir" => state_dir = arguments.next().map(PathBuf::from),
            "--agent" => agent = arguments.next().map(PathBuf::from),
            other => bail!("unknown argument {other}"),
        }
    }
    let state_dir = expand_home(state_dir.context("--state-dir is required")?);
    reconcile(&state_dir)?;
    match command.as_str() {
        "reconcile" => Ok(()),
        "supervise" => {
            let agent = agent.context("--agent is required for supervise")?;
            let status = Command::new(agent).status()?;
            if status.success() {
                Ok(())
            } else {
                let _ = reconcile(&state_dir);
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        other => bail!("unknown command {other}"),
    }
}

fn expand_home(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    else {
        return path;
    };
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(rest))
        .unwrap_or(path)
}

fn reconcile(state_dir: &Path) -> anyhow::Result<()> {
    let path = state_dir.join("pending-update.json");
    let Ok(contents) = std::fs::read(&path) else {
        return Ok(());
    };
    let mut pending: PendingUpdate = serde_json::from_slice(&contents)?;
    match pending.phase.as_str() {
        "pending" => apply(&mut pending)?,
        "awaiting_health" => {
            let elapsed = now_epoch_seconds().saturating_sub(pending.applied_at_epoch_seconds);
            if pending.attempts >= 1 || elapsed >= pending.health_timeout_seconds as i64 {
                rollback(&mut pending)?;
            } else {
                pending.attempts += 1;
            }
        }
        "healthy" | "rolled_back" => return Ok(()),
        phase => bail!("unknown OTA phase {phase}"),
    }
    atomic_json(&path, &pending)
}

fn apply(pending: &mut PendingUpdate) -> anyhow::Result<()> {
    if digest_file(&pending.artifact_path)? != pending.sha256 {
        bail!("staged OTA artifact failed sha256 verification");
    }
    let parent = pending
        .target_path
        .parent()
        .context("agent executable has no parent directory")?;
    let file_name = pending
        .target_path
        .file_name()
        .context("agent executable has no file name")?
        .to_string_lossy();
    let next = parent.join(format!(".{file_name}.next"));
    let previous = parent.join(format!("{file_name}.previous"));
    copy_synced(&pending.artifact_path, &next)?;
    if pending.target_path.exists() {
        copy_synced(&pending.target_path, &previous)?;
    }
    replace_file(&next, &pending.target_path)?;
    pending.phase = "awaiting_health".to_string();
    pending.attempts = 0;
    pending.applied_at_epoch_seconds = now_epoch_seconds();
    Ok(())
}

fn rollback(pending: &mut PendingUpdate) -> anyhow::Result<()> {
    let parent = pending
        .target_path
        .parent()
        .context("agent executable has no parent directory")?;
    let file_name = pending
        .target_path
        .file_name()
        .context("agent executable has no file name")?
        .to_string_lossy();
    let previous = parent.join(format!("{file_name}.previous"));
    if !previous.exists() {
        bail!("OTA health check failed and no rollback executable exists");
    }
    let rollback_copy = parent.join(format!(".{file_name}.rollback"));
    copy_synced(&previous, &rollback_copy)?;
    replace_file(&rollback_copy, &pending.target_path)?;
    pending.phase = "rolled_back".to_string();
    Ok(())
}

fn copy_synced(source: &Path, target: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, target)
        .with_context(|| format!("copy {} to {}", source.display(), target.display()))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(target)?
        .sync_all()?;
    Ok(())
}

fn now_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_failed_health_roll_back_the_binary() {
        let root = std::env::temp_dir().join(format!(
            "radiochron-update-{}-{}",
            std::process::id(),
            now_epoch_seconds()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("agent");
        let artifact = root.join("artifact");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&artifact, b"new").unwrap();
        let mut pending = PendingUpdate {
            schema_version: 1,
            device_id: "device".into(),
            profile_revision: 1,
            version: "2".into(),
            artifact_path: artifact,
            target_path: target.clone(),
            sha256: update_fs::hex(&Sha256::digest(b"new")),
            health_timeout_seconds: 0,
            phase: "pending".into(),
            attempts: 0,
            applied_at_epoch_seconds: 0,
        };
        apply(&mut pending).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        rollback(&mut pending).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(root);
    }
}
