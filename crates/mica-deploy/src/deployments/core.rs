//! Installing a core set, and settling its trial.
//!
//! The running system's half of `core_state`: a new set is published beside
//! the current one and given its trial; the health gate's confirmation then
//! makes it current and releases the old one, or drops it once its trial is
//! spent. A core set and a deployment are never on trial together, so a boot
//! that does not reach healthy has one cause.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use super::*;
use crate::components::{Artifact, authenticate_deployment};
use crate::core_set::{self, CoreSet};
use crate::core_state::{self, CoreBoot, CoreState};

/// The device a core set must suit: its architecture, and the product and
/// root it would be composed over.
pub struct CoreTarget<'a> {
    pub arch: &'a str,
    pub features: &'a [String],
    pub root_level: u64,
}

/// One held set, for status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreSetStatus {
    pub id: String,
    pub channel: String,
    pub generation: u64,
    pub version: String,
    /// The packages this device composes from it.
    pub selected: Vec<String>,
}

/// The sets a device holds and what it booted.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStatus {
    pub current: Option<CoreSetStatus>,
    pub pending: Option<CoreSetStatus>,
    /// The boots the pending set has left.
    pub attempts_left: Option<u8>,
    pub boot: CoreBoot,
}

/// What confirming a boot did to the core sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CoreSettlement {
    /// Nothing was pending, or the pending set still has its trial ahead.
    Unchanged,
    /// The set on trial reached healthy and is now the current one.
    Committed,
    /// The pending set spent its trial without a healthy boot and is gone.
    Dropped,
}

impl DeploymentStore {
    fn core_objects<'a>(&self, set: &'a CoreSet) -> Vec<(PathBuf, &'a Artifact)> {
        let mut paths = Vec::new();
        for core in &set.components {
            let directory = self.system.join(format!("cores/{}", core.id));
            paths.push((directory.join("core.img"), &core.content.image));
            paths.push((directory.join("core.roothash.p7s"), &core.content.signature));
        }
        paths
    }

    /// The sets held and the trial, as the running system reports them.
    pub fn core_status(
        &self,
        keys: &[[u8; 32]],
        target: &CoreTarget,
        boot: CoreBoot,
    ) -> Result<CoreStatus> {
        let state = core_state::read_state(&self.system)?;
        let describe = |id: &String| -> Result<CoreSetStatus> {
            let (set, _) = core_state::load_set(&self.system, id, keys, target.arch)?;
            // A set that no longer selects here is still reported, with nothing selected.
            let selected = core_set::select(&set, target.features, target.root_level)
                .map(|chosen| chosen.iter().map(|core| core.package.clone()).collect())
                .unwrap_or_default();
            Ok(CoreSetStatus {
                id: id.clone(),
                channel: set.channel,
                generation: set.generation,
                version: set.version,
                selected,
            })
        };
        Ok(CoreStatus {
            current: state.current.as_ref().map(describe).transpose()?,
            pending: state.pending.as_ref().map(describe).transpose()?,
            attempts_left: state
                .pending
                .as_ref()
                .and_then(|_| core_state::read_trial(&self.meta))
                .map(|trial| trial.attempts_left),
            boot,
        })
    }

    /// Publish a signed core set beside the current one and start its trial.
    ///
    /// The caller holds the DATA transaction lock and a writable SYSTEM.
    /// Object sources are addressed only by authenticated SHA-256 digests.
    pub fn install_core(
        &self,
        envelope: &[u8],
        keys: &[[u8; 32]],
        target: &CoreTarget,
        objects: &Path,
    ) -> Result<CoreState> {
        let set = core_set::authenticate(envelope, keys)?;
        ensure!(
            set.arch == target.arch,
            "core set targets another architecture"
        );
        let id = set.id()?;
        ensure!(
            self.effective_state()?.candidate.is_none(),
            "a deployment is on trial; confirm it before a core update"
        );
        let mut state = core_state::read_state(&self.system)?;
        ensure!(state.pending.is_none(), "another core set is pending");
        ensure!(
            state.current.as_deref() != Some(id.as_str()),
            "core set is installed"
        );
        if let Some(current) = &state.current {
            let (held, _) = core_state::load_set(&self.system, current, keys, target.arch)?;
            // Generations order one channel. Another channel's set is the
            // operator's switch, and starts that channel's own line.
            ensure!(
                held.channel != set.channel || set.generation > held.generation,
                "core set generation is not newer"
            );
        }
        core_set::select(&set, target.features, target.root_level)
            .context("the core set does not suit this device")?;

        let files = self.core_objects(&set);
        let mut bytes = envelope.len() as u64;
        for (path, artifact) in &files {
            if path.symlink_metadata().is_ok() {
                verify_file(path, artifact)?;
                continue;
            }
            verify_file(&objects.join(&artifact.sha256), artifact)?;
            bytes += artifact.bytes;
        }
        directory(&self.system)?;
        let space = rustix::fs::statvfs(&self.system)?;
        ensure!(
            space.f_bavail * space.f_frsize >= bytes + self.reserves.system(),
            "insufficient destination space: {}",
            self.system.display()
        );
        ensure!(
            space.f_files == 0 || space.f_favail >= 32,
            "insufficient destination inodes"
        );
        for (path, artifact) in &files {
            publish_object(&objects.join(&artifact.sha256), path, artifact)?;
        }
        for core in &set.components {
            let path = self.system.join(format!("cores/{}/core.roothash", core.id));
            if path.try_exists()? {
                ensure!(
                    read_bounded(&path, 64)? == core.content.root_hash.as_bytes(),
                    "existing root hash differs"
                );
            } else {
                atomic_write(&path, core.content.root_hash.as_bytes())?;
            }
        }
        directory(&self.system.join(core_state::SETS_DIR))?;
        let published = core_state::set_path(&self.system, &id);
        if published.try_exists()? {
            ensure!(
                read_bounded(&published, core_state::MAX_ENVELOPE_BYTES)? == envelope,
                "core set ID was reused"
            );
        } else {
            atomic_write(&published, envelope)?;
        }
        sync_dir(&self.system)?;
        // The trial first: a pending set with no trial is never tried, a
        // trial with no pending set is ignored.
        core_state::start_trial(&self.meta, &id)?;
        state.pending = Some(id);
        core_state::write_state(&self.system, &state)?;
        Ok(state)
    }

    /// Whether confirming this boot changes the core sets, and so needs a
    /// writable SYSTEM.
    pub fn core_needs_settling(&self, boot: &CoreBoot) -> Result<bool> {
        Ok(self.core_settlement(boot)? != CoreSettlement::Unchanged)
    }

    fn core_settlement(&self, boot: &CoreBoot) -> Result<CoreSettlement> {
        let state = core_state::read_state(&self.system)?;
        let Some(pending) = &state.pending else {
            return Ok(CoreSettlement::Unchanged);
        };
        Ok(if boot.core_set_id.as_ref() == Some(pending) {
            CoreSettlement::Committed
        } else if core_state::trial_is_over(&self.meta, pending) {
            CoreSettlement::Dropped
        } else {
            CoreSettlement::Unchanged
        })
    }

    /// Settle the pending core set against a boot that reached healthy: the
    /// set that was booted becomes current, one whose trial is over is
    /// dropped, and what nothing names any more is released.
    ///
    /// The caller holds the transaction lock and, when
    /// [`Self::core_needs_settling`] says so, a writable SYSTEM.
    pub fn settle_core(&self, boot: &CoreBoot, keys: &[[u8; 32]]) -> Result<CoreSettlement> {
        let settlement = self.core_settlement(boot)?;
        if settlement == CoreSettlement::Unchanged {
            return Ok(settlement);
        }
        let mut state = core_state::read_state(&self.system)?;
        let pending = state.pending.take().context("pending core set vanished")?;
        let released = if settlement == CoreSettlement::Committed {
            state.current.replace(pending)
        } else {
            Some(pending)
        };
        // The state first: from here the released set is unreferenced, and an
        // interruption leaves only files the next settlement or collection removes.
        core_state::write_state(&self.system, &state)?;
        core_state::end_trial(&self.meta)?;
        if let Some(id) = released {
            let path = core_state::set_path(&self.system, &id);
            if path.symlink_metadata().is_ok() {
                std::fs::remove_file(&path)?;
                sync_dir(&self.system.join(core_state::SETS_DIR))?;
            }
        }
        self.release_unreferenced_cores(keys)?;
        Ok(settlement)
    }

    /// The core components the held sets name.
    pub(super) fn core_set_components(&self, keys: &[[u8; 32]]) -> Result<BTreeSet<String>> {
        let state = core_state::read_state(&self.system)?;
        let mut kept = BTreeSet::new();
        for id in [&state.current, &state.pending].into_iter().flatten() {
            let envelope = read_bounded(
                &core_state::set_path(&self.system, id),
                core_state::MAX_ENVELOPE_BYTES,
            )?;
            let set = core_set::authenticate(&envelope, keys)?;
            kept.extend(set.components.into_iter().map(|core| core.id));
        }
        Ok(kept)
    }

    /// Remove every core component neither a held set nor an installed
    /// deployment names.
    fn release_unreferenced_cores(&self, keys: &[[u8; 32]]) -> Result<usize> {
        let mut kept = self.core_set_components(keys)?;
        let descriptors = self.system.join("deployments");
        if descriptors.symlink_metadata().is_ok() {
            for entry in std::fs::read_dir(&descriptors)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let deployment = authenticate_deployment(&read_bounded(&path, 24576)?, keys)?;
                kept.extend(deployment.core.into_iter().map(|core| core.id));
            }
        }
        let cores = self.system.join("cores");
        if cores.symlink_metadata().is_err() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in std::fs::read_dir(&cores)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid object name"))?;
            valid_id(&name)?;
            ensure!(entry.file_type()?.is_dir(), "object directory is not real");
            if kept.contains(&name) {
                continue;
            }
            for child in std::fs::read_dir(entry.path())? {
                let child = child?;
                ensure!(child.file_type()?.is_file(), "unexpected object child");
                std::fs::remove_file(child.path())?;
                removed += 1;
            }
            std::fs::remove_dir(entry.path())?;
        }
        sync_dir(&cores)?;
        Ok(removed)
    }
}
