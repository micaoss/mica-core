//! Installing a set, and settling its trial.
//!
//! The running system's half of `set_state`: a new set is published beside
//! the current one of its kind and given its trial; the health gate's
//! confirmation then makes it current and releases the old one, or drops it
//! once its trial is spent. A set and a deployment are never on trial
//! together, so a boot that does not reach healthy has one cause.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use super::*;
use crate::components::Artifact;
use crate::core_set::{self, CoreBoot};
use crate::set_state::{self, SetState};
use crate::update_set::{self, SetKind, UpdateSet};

/// The device a set must suit: its architecture, and what the kind's own
/// selection asks of it. Core asks for the product's features and the level of
/// the root it would be composed over.
pub struct SetTarget<'a> {
    pub arch: &'a str,
    pub features: &'a [String],
    pub root_level: u64,
}

/// One held set, for status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldSet {
    pub id: String,
    pub channel: String,
    pub generation: u64,
    pub version: String,
    /// The packages this device takes from it.
    pub selected: Vec<String>,
}

/// The core sets a device holds and what it booted.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreSetsStatus {
    pub current: Option<HeldSet>,
    pub pending: Option<HeldSet>,
    /// The boots the pending set has left.
    pub attempts_left: Option<u8>,
    pub boot: CoreBoot,
}

/// The sets a device holds, by kind: `status`'s `sets`.
#[derive(Debug, Serialize)]
pub struct SetsStatus {
    pub core: CoreSetsStatus,
}

/// What confirming a boot did to the sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SetSettlement {
    /// Nothing was pending, or the pending set still has its trial ahead.
    Unchanged,
    /// The set on trial reached healthy and is now the current one.
    Committed,
    /// The pending set spent its trial without a healthy boot and is gone.
    Dropped,
}

impl DeploymentStore {
    /// Where each object of `set` is held once installed.
    pub(crate) fn set_objects<'a>(&self, set: &'a UpdateSet) -> Vec<(PathBuf, &'a Artifact)> {
        let mut paths = Vec::new();
        for core in &set.components {
            let directory = set_state::component_dir(&self.system, set.kind, &core.id);
            paths.push((directory.join("core.img"), &core.content.image));
            paths.push((directory.join("core.roothash.p7s"), &core.content.signature));
        }
        paths
    }

    /// The sets held and the trial, as the running system reports them.
    pub fn sets_status(
        &self,
        keys: &[[u8; 32]],
        target: &SetTarget,
        boot: CoreBoot,
    ) -> Result<SetsStatus> {
        let kind = SetKind::Core;
        let state = set_state::read_state(&self.system, kind)?;
        let describe = |id: &String| -> Result<HeldSet> {
            let (set, _) = set_state::load_set(&self.system, kind, id, keys, target.arch)?;
            // A set that no longer selects here is still reported, with nothing selected.
            let selected = core_set::select(&set, target.features, target.root_level)
                .map(|chosen| chosen.iter().map(|core| core.package.clone()).collect())
                .unwrap_or_default();
            Ok(HeldSet {
                id: id.clone(),
                channel: set.channel,
                generation: set.generation,
                version: set.version,
                selected,
            })
        };
        Ok(SetsStatus {
            core: CoreSetsStatus {
                current: state.current.as_ref().map(describe).transpose()?,
                pending: state.pending.as_ref().map(describe).transpose()?,
                attempts_left: state
                    .pending
                    .as_ref()
                    .and_then(|_| set_state::read_trial(&self.meta))
                    .filter(|trial| trial.kind == kind)
                    .map(|trial| trial.attempts_left),
                boot,
            },
        })
    }

    /// Publish a signed set beside the current one of its kind and start its
    /// trial. The kind is the one the set was signed as.
    ///
    /// The caller holds the DATA transaction lock and a writable SYSTEM.
    /// Object sources are addressed only by authenticated SHA-256 digests.
    pub fn install_set(
        &self,
        envelope: &[u8],
        keys: &[[u8; 32]],
        target: &SetTarget,
        objects: &Path,
    ) -> Result<SetState> {
        let set = update_set::authenticate(envelope, keys)?;
        let kind = set.kind;
        ensure!(set.arch == target.arch, "set targets another architecture");
        let id = set.id()?;
        ensure!(
            self.effective_state()?.candidate.is_none(),
            "a deployment is on trial; confirm it before a set update"
        );
        let mut state = set_state::read_state(&self.system, kind)?;
        ensure!(state.pending.is_none(), "another set is pending");
        ensure!(
            state.current.as_deref() != Some(id.as_str()),
            "set is installed"
        );
        if let Some(current) = &state.current {
            let (held, _) = set_state::load_set(&self.system, kind, current, keys, target.arch)?;
            // Generations order one channel. Another channel's set is the
            // operator's switch, and starts that channel's own line.
            ensure!(
                held.channel != set.channel || set.generation > held.generation,
                "set generation is not newer"
            );
        }
        match kind {
            SetKind::Core => core_set::select(&set, target.features, target.root_level)
                .context("the set does not suit this device")?,
        };

        let files = self.set_objects(&set);
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
            let path = set_state::component_dir(&self.system, kind, &core.id).join("core.roothash");
            if path.try_exists()? {
                ensure!(
                    read_bounded(&path, 64)? == core.content.root_hash.as_bytes(),
                    "existing root hash differs"
                );
            } else {
                atomic_write(&path, core.content.root_hash.as_bytes())?;
            }
        }
        directory(&set_state::kind_dir(&self.system, kind))?;
        let published = set_state::set_path(&self.system, kind, &id);
        if published.try_exists()? {
            ensure!(
                read_bounded(&published, set_state::MAX_ENVELOPE_BYTES)? == envelope,
                "set ID was reused"
            );
        } else {
            atomic_write(&published, envelope)?;
        }
        sync_dir(&self.system)?;
        // The trial first: a pending set with no trial is never tried, a
        // trial with no pending set is ignored.
        set_state::start_trial(&self.meta, kind, &id)?;
        state.pending = Some(id);
        set_state::write_state(&self.system, kind, &state)?;
        Ok(state)
    }

    /// Whether confirming this boot changes the sets, and so needs a
    /// writable SYSTEM.
    pub fn sets_need_settling(&self, boot: &CoreBoot) -> Result<bool> {
        Ok(self.settlement(boot)? != SetSettlement::Unchanged)
    }

    /// What a healthy boot means for the pending set. For core, the one kind:
    /// the set the runkit composed is the one that was tried.
    fn settlement(&self, boot: &CoreBoot) -> Result<SetSettlement> {
        let state = set_state::read_state(&self.system, SetKind::Core)?;
        let Some(pending) = &state.pending else {
            return Ok(SetSettlement::Unchanged);
        };
        Ok(if &boot.core_set_id == pending {
            SetSettlement::Committed
        } else if set_state::trial_is_over(&self.meta, SetKind::Core, pending) {
            SetSettlement::Dropped
        } else {
            SetSettlement::Unchanged
        })
    }

    /// Settle the pending set against a boot that reached healthy: the
    /// set that was booted becomes current, one whose trial is over is
    /// dropped, and what nothing names any more is released.
    ///
    /// The caller holds the transaction lock and, when
    /// [`Self::sets_need_settling`] says so, a writable SYSTEM.
    pub fn settle_sets(&self, boot: &CoreBoot, keys: &[[u8; 32]]) -> Result<SetSettlement> {
        let kind = SetKind::Core;
        let settlement = self.settlement(boot)?;
        if settlement == SetSettlement::Unchanged {
            return Ok(settlement);
        }
        let mut state = set_state::read_state(&self.system, kind)?;
        let pending = state.pending.take().context("pending set vanished")?;
        let released = if settlement == SetSettlement::Committed {
            state.current.replace(pending)
        } else {
            Some(pending)
        };
        // The state first: from here the released set is unreferenced, and an
        // interruption leaves only files the next settlement or collection removes.
        set_state::write_state(&self.system, kind, &state)?;
        set_state::end_trial(&self.meta)?;
        if let Some(id) = released {
            let path = set_state::set_path(&self.system, kind, &id);
            if path.symlink_metadata().is_ok() {
                std::fs::remove_file(&path)?;
                sync_dir(&set_state::kind_dir(&self.system, kind))?;
            }
        }
        self.release_unreferenced(kind, keys)?;
        Ok(settlement)
    }

    /// The components the held sets of `kind` name.
    fn held_components(&self, kind: SetKind, keys: &[[u8; 32]]) -> Result<BTreeSet<String>> {
        let state = set_state::read_state(&self.system, kind)?;
        let mut kept = BTreeSet::new();
        for id in [&state.current, &state.pending].into_iter().flatten() {
            let envelope = read_bounded(
                &set_state::set_path(&self.system, kind, id),
                set_state::MAX_ENVELOPE_BYTES,
            )?;
            let set = update_set::authenticate(&envelope, keys)?;
            kept.extend(set.components.into_iter().map(|core| core.id));
        }
        Ok(kept)
    }

    /// Remove every component object of `kind` no held set names.
    fn release_unreferenced(&self, kind: SetKind, keys: &[[u8; 32]]) -> Result<usize> {
        let kept = self.held_components(kind, keys)?;
        let cores = set_state::objects_dir(&self.system, kind);
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
