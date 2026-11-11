//! Whether a rollback took.
//!
//! A rollback is staged in one boot and takes effect in the next, which the
//! operator starts separately. The target is recorded on DATA when the
//! rollback is staged, with the boot it was staged in; the first status read
//! in a later boot compares the booted deployment with it and keeps the
//! outcome for that boot alone. A boot after that drops the record.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::deployment::Status;

/// Where the record lives: DATA, so it outlives the reboot it is about.
pub const DEFAULT_RECORD_PATH: &str = "/var/lib/mica/update/rollback.json";
/// The kernel's identity of the running boot.
pub const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    /// The boot the rollback was staged in.
    staged_boot: String,
    target: String,
    target_generation: Option<u64>,
    target_version: Option<String>,
    /// The boot the outcome was decided in, and the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evaluated: Option<(String, Value)>,
}

/// The record and the boot identity it is compared against.
pub struct RollbackRecord {
    path: PathBuf,
    boot_id: PathBuf,
}

impl RollbackRecord {
    pub fn new(path: PathBuf, boot_id: PathBuf) -> Self {
        Self { path, boot_id }
    }

    pub fn production() -> Self {
        Self::new(DEFAULT_RECORD_PATH.into(), BOOT_ID_PATH.into())
    }

    fn current_boot(&self) -> Result<String> {
        Ok(std::fs::read_to_string(&self.boot_id)
            .with_context(|| format!("read {}", self.boot_id.display()))?
            .trim()
            .to_string())
    }

    /// Record that a rollback to `target` was staged in this boot. `status`
    /// is the deployment status read before the rollback, which names the
    /// target's generation and version.
    pub fn stage(&self, status: &Status, target: &str) -> Result<()> {
        let deployment = status.deployments.iter().find(|entry| entry.id == target);
        let record = Record {
            staged_boot: self.current_boot()?,
            target: target.to_string(),
            target_generation: deployment.map(|entry| entry.generation),
            target_version: deployment.map(|entry| entry.version.clone()),
            evaluated: None,
        };
        write(&self.path, &record)
    }

    /// The outcome to report in this boot: `None` when no rollback was
    /// staged, when this is still the boot it was staged in, or when its
    /// outcome was reported in an earlier boot (the record is then dropped).
    pub fn outcome(&self, status: &Status) -> Option<Value> {
        let record: Record = match std::fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(%error, path = %self.path.display(), "unreadable rollback record dropped");
                    let _ = std::fs::remove_file(&self.path);
                    return None;
                }
            },
            Err(_) => return None,
        };
        let boot = match self.current_boot() {
            Ok(boot) => boot,
            Err(error) => {
                tracing::warn!(%error, "boot identity unavailable; rollback outcome not decided");
                return None;
            }
        };
        if let Some((evaluated_in, outcome)) = &record.evaluated {
            if *evaluated_in == boot {
                return Some(outcome.clone());
            }
            let _ = std::fs::remove_file(&self.path);
            return None;
        }
        if record.staged_boot == boot {
            return None;
        }
        let outcome = evaluate(&record, status);
        let decided = Record {
            evaluated: Some((boot, outcome.clone())),
            ..record
        };
        if let Err(error) = write(&self.path, &decided) {
            tracing::warn!(%error, "rollback outcome not recorded");
        }
        Some(outcome)
    }
}

/// The booted deployment against the recorded target. When it is another
/// one, the reason is what the deployment state says of the target.
fn evaluate(record: &Record, status: &Status) -> Value {
    let booted = &status.boot.deployment_id;
    let mut outcome = json!({
        "target": record.target,
        "targetGeneration": record.target_generation,
        "targetVersion": record.target_version,
    });
    if *booted == record.target {
        outcome["outcome"] = json!("rolled_back");
        return outcome;
    }
    let entry = |id: &str| status.deployments.iter().find(|entry| entry.id == id);
    let reason = if status.state.failed.contains(&record.target) {
        Some("target_failed")
    } else {
        match entry(&record.target) {
            None => Some("target_missing"),
            Some(target) if target.tries_left == Some(0) => Some("target_exhausted"),
            Some(_) => None,
        }
    };
    outcome["outcome"] = json!("did_not_take");
    outcome["booted"] = json!(booted);
    outcome["bootedGeneration"] = json!(entry(booted).map(|entry| entry.generation));
    outcome["bootedVersion"] = json!(entry(booted).map(|entry| &entry.version));
    outcome["reason"] = json!(reason);
    outcome
}

fn write(path: &Path, record: &Record) -> Result<()> {
    crate::fswrite::write_config_if_changed(path, &serde_json::to_string(record)?, 0o644)?;
    Ok(())
}

#[cfg(test)]
mod tests;
