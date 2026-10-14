//! The published snapshots on disk.

use anyhow::{Context, bail};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

use super::*;

/// One stored snapshot, as the list route describes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSummary {
    /// The store's sequence number, and the id in the routes.
    pub id: u64,
    /// The snapshot's size on disk.
    pub bytes: u64,
    /// The snapshot's `collectedAt`, when it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<String>,
    /// The snapshot's `schemaVersion`, when it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
    /// The snapshot's machine id, when it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}

/// The bounded on-disk store.
///
/// Layout, all of it under one root (`/mica/diagnostics` on a device, a
/// temporary directory in tests):
///
/// ```text
/// <root>/<id>.json            a published snapshot
/// <root>/.staging-<id>.json   one being written; never listed
/// ```
///
/// Absence of the root is a defined state — the shipped state of every
/// device — and it is created on the first publish, never at start-up.
pub struct SnapshotStore {
    pub(super) root: PathBuf,
    pub(super) max_snapshots: usize,
    pub(super) max_total_bytes: u64,
}

impl SnapshotStore {
    /// A store rooted at `root`, on the shipped caps.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_snapshots: MAX_SNAPSHOTS,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }

    /// The store at [`DEFAULT_ROOT`]. A path and no syscall.
    #[must_use]
    pub fn at_default() -> Self {
        Self::new(DEFAULT_ROOT)
    }

    /// Override the retention caps, for the tests that prove them.
    #[cfg(test)]
    #[must_use]
    pub fn with_caps(mut self, max_snapshots: usize, max_total_bytes: u64) -> Self {
        self.max_snapshots = max_snapshots;
        self.max_total_bytes = max_total_bytes;
        self
    }

    /// Where the store lives, for the tests that look at the directory.
    #[cfg(test)]
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(super) fn path_of(&self, id: u64) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// Every published snapshot, oldest first.
    pub fn list(&self) -> anyhow::Result<Vec<SnapshotSummary>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| format!("read {}", self.root.display()));
            }
        };
        let mut summaries = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("read {}", self.root.display()))?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|stem| stem.parse::<u64>().ok())
            else {
                continue;
            };
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let (collected_at, schema_version, machine_id) = fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .map(|value| {
                    (
                        value
                            .get("collectedAt")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        value.get("schemaVersion").and_then(Value::as_u64),
                        value
                            .pointer("/system/machineId/id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    )
                })
                .unwrap_or_default();
            summaries.push(SnapshotSummary {
                id,
                bytes: metadata.len(),
                collected_at,
                schema_version,
                machine_id,
            });
        }
        summaries.sort_by_key(|summary| summary.id);
        Ok(summaries)
    }

    /// The bytes of snapshot `id`, or `None` when there is no such snapshot.
    pub fn read(&self, id: u64) -> anyhow::Result<Option<Vec<u8>>> {
        match fs::read(self.path_of(id)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read snapshot {id}")),
        }
    }

    /// Remove snapshot `id`; `false` when there was none.
    pub fn delete(&self, id: u64) -> anyhow::Result<bool> {
        match fs::remove_file(self.path_of(id)) {
            Ok(()) => {
                mica_fs::sync_dir(&self.root)?;
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| format!("delete snapshot {id}")),
        }
    }

    /// Publish `snapshot`: refuse it above [`MAX_SNAPSHOT_BYTES`], make room
    /// under the caps by removing the oldest, write it to a staging file,
    /// fsync, rename into place, fsync the directory.
    ///
    /// Whole or not at all: until the rename the store lists nothing new,
    /// and a failure anywhere leaves at most a staging file the next publish
    /// sweeps.
    pub fn publish(&self, snapshot: &Value) -> anyhow::Result<SnapshotSummary> {
        let bytes = serde_json::to_vec(snapshot).context("encode snapshot")?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            bail!(
                "the snapshot is {} bytes, above the {MAX_SNAPSHOT_BYTES} byte cap; nothing was written",
                bytes.len()
            );
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create {}", self.root.display()))?;
        self.sweep_staging();
        let existing = self.list()?;
        let id = existing.iter().map(|summary| summary.id).max().unwrap_or(0) + 1;
        let mut count = existing.len();
        let mut total: u64 = existing.iter().map(|summary| summary.bytes).sum();
        for old in &existing {
            if count < self.max_snapshots && total + bytes.len() as u64 <= self.max_total_bytes {
                break;
            }
            fs::remove_file(self.path_of(old.id))
                .with_context(|| format!("evict snapshot {}", old.id))?;
            count -= 1;
            total -= old.bytes;
        }
        let staging = self.root.join(format!(".staging-{id}.json"));
        let target = self.path_of(id);
        let written = mica_fs::Replace::via(&target, staging.clone()).write(&bytes);
        if let Err(err) = written {
            let _ = fs::remove_file(&staging);
            return Err(err).with_context(|| format!("publish snapshot {id}"));
        }
        Ok(SnapshotSummary {
            id,
            bytes: bytes.len() as u64,
            collected_at: snapshot
                .get("collectedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            schema_version: snapshot.get("schemaVersion").and_then(Value::as_u64),
            machine_id: snapshot
                .pointer("/system/machineId/id")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// Remove staging files a crashed publish left behind.
    pub(super) fn sweep_staging(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".staging-"))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
