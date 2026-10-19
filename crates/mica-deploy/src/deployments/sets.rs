//! Installing a set, and settling its trial.
//!
//! The running system's half of `set_state`: a new set is published beside
//! the current one of its kind and given its trial, alone or paired with a
//! new deployment; the health gate's confirmation then makes it current and
//! binds the deployment that ran the old one to it, or drops it once its
//! trial is spent. A set alone and a deployment are never on trial together,
//! so a boot that does not reach healthy has one cause.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    pub root: crate::components::RootFacts<'a>,
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
    /// The deployment the pending set is paired with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired: Option<String>,
    /// Each other retained deployment's own set, by deployment id.
    pub bound: BTreeMap<String, HeldSet>,
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
    pub(crate) fn set_objects<'a>(&self, set: &'a UpdateSet) -> Vec<(Place, &'a Artifact)> {
        let mut paths = Vec::new();
        for core in &set.components {
            let directory = set_state::component_dir(&self.system, set.kind, &core.id);
            paths.push((
                match self.ubi() {
                    Some(_) => Place::Volume(format!("c-{}", core.id)),
                    None => Place::File(directory.join("core.img")),
                },
                &core.content.image,
            ));
            paths.push((
                Place::File(directory.join("core.roothash.p7s")),
                &core.content.signature,
            ));
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
            let selected = core_set::select(&set, target.features, &target.root)
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
                paired: state.paired().map(str::to_owned),
                bound: state
                    .bound
                    .iter()
                    .map(|(deployment, id)| Ok((deployment.clone(), describe(id)?)))
                    .collect::<Result<_>>()?,
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
            SetKind::Core => core_set::select(&set, target.features, &target.root)
                .context("the set does not suit this device")?,
        };

        self.publish_set(&set, envelope, objects)?;
        // A set alone: no pairing is left to make it one, and no deployment
        // that is gone keeps a set.
        let retained = self.retained_deployments(None)?;
        state.pair = None;
        state
            .bound
            .retain(|deployment, _| retained.contains(deployment));
        // The trial first: a pending set with no trial is never tried, a
        // trial with no pending set is ignored.
        set_state::start_trial(&self.meta, kind, &id)?;
        state.pending = Some(id);
        set_state::write_state(&self.system, kind, &state)?;
        Ok(state)
    }

    /// Publish a signed set's objects and envelope on SYSTEM, from `objects`
    /// or what is already held, within the reserve. Nothing names it yet.
    pub(crate) fn publish_set(
        &self,
        set: &UpdateSet,
        envelope: &[u8],
        objects: &Path,
    ) -> Result<()> {
        let kind = set.kind;
        let files = self.set_objects(set);
        let mut bytes = envelope.len() as u64;
        let mut lebs = 0;
        for (path, artifact) in &files {
            if self.held(path)? {
                self.verify_held(path, artifact)?;
                continue;
            }
            verify_file(&objects.join(&artifact.sha256), artifact)?;
            match (path, self.ubi()) {
                (Place::Volume(_), Some(ubi)) => lebs += crate::ubi::lebs(ubi, artifact.bytes),
                _ => bytes += artifact.bytes,
            }
        }
        directory(&self.system)?;
        let space = rustix::fs::statvfs(&self.system)?;
        // On UBI the reserve is kept in the device's free erase blocks.
        let reserve = match self.ubi() {
            Some(ubi) => {
                ensure!(
                    ubi.available_lebs()? >= lebs + crate::ubi::lebs(ubi, self.reserves.system()),
                    "insufficient UBI space"
                );
                0
            }
            None => self.reserves.system(),
        };
        ensure!(
            space.f_bavail * space.f_frsize >= bytes + reserve,
            "insufficient destination space: {}",
            self.system.display()
        );
        ensure!(
            space.f_files == 0 || space.f_favail >= 32,
            "insufficient destination inodes"
        );
        for (path, artifact) in &files {
            self.publish(&objects.join(&artifact.sha256), path, artifact)?;
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
        let published = set_state::set_path(&self.system, kind, &set.id()?);
        if published.try_exists()? {
            ensure!(
                read_bounded(&published, set_state::MAX_ENVELOPE_BYTES)? == envelope,
                "set ID was reused"
            );
        } else {
            atomic_write(&published, envelope)?;
        }
        sync_dir(&self.system)?;
        Ok(())
    }

    /// The deployments a confirmation of `booted` leaves: every entry with a
    /// boot left, and the one booted.
    fn retained_deployments(&self, booted: Option<&str>) -> Result<BTreeSet<String>> {
        Ok(self
            .entries()?
            .into_iter()
            .filter(|entry| entry.tries_left != Some(0) || Some(entry.id.as_str()) == booted)
            .map(|entry| entry.id)
            .collect())
    }

    /// Whether confirming a boot of the deployment `booted` changes the
    /// sets, and so needs a writable SYSTEM.
    pub fn sets_need_settling(&self, booted: &str, boot: &CoreBoot) -> Result<bool> {
        let held = set_state::read_state(&self.system, SetKind::Core)?;
        let (_, state) = self.settlement(booted, boot)?;
        Ok(state != held)
    }

    /// What a healthy boot of the deployment `booted` means for the sets of
    /// core, the one kind, and the state it leaves.
    ///
    /// The set the runkit composed is the one that was tried: a pending set
    /// booted becomes current. A pair whose deployment is no longer retained,
    /// or a set alone whose trial is over, is dropped. A deployment that is
    /// confirmed runs on `current`: when it booted a set bound to it, that set
    /// becomes current again. Whenever `current` changes, each other retained
    /// deployment that is not bound is bound to the set it ran, the old
    /// `current`, and bindings of deployments no longer retained are dropped.
    fn settlement(&self, booted: &str, boot: &CoreBoot) -> Result<(SetSettlement, SetState)> {
        let kind = SetKind::Core;
        let mut state = set_state::read_state(&self.system, kind)?;
        let retained = self.retained_deployments(Some(booted))?;
        let paired = state.paired().map(str::to_owned);
        let settlement = match &state.pending {
            None => SetSettlement::Unchanged,
            Some(pending) if &boot.core_set_id == pending => SetSettlement::Committed,
            Some(pending) => match &paired {
                Some(deployment) if deployment != booted && retained.contains(deployment) => {
                    SetSettlement::Unchanged
                }
                Some(_) => SetSettlement::Dropped,
                None if set_state::trial_is_over(&self.meta, kind, pending) => {
                    SetSettlement::Dropped
                }
                None => SetSettlement::Unchanged,
            },
        };
        let make_current = |state: &mut SetState, id: String| {
            if let Some(old) = state.current.replace(id) {
                for deployment in &retained {
                    if deployment != booted && !state.bound.contains_key(deployment) {
                        state.bound.insert(deployment.clone(), old.clone());
                    }
                }
            }
        };
        match settlement {
            SetSettlement::Committed => {
                let pending = state.pending.take().context("pending set vanished")?;
                make_current(&mut state, pending);
            }
            SetSettlement::Dropped => state.pending = None,
            SetSettlement::Unchanged => {}
        }
        if let Some(own) = state.bound.remove(booted)
            && state.current.as_ref() != Some(&own)
        {
            make_current(&mut state, own);
        }
        if state.pending.is_none() {
            state.pair = None;
        }
        let current = state.current.clone();
        state
            .bound
            .retain(|deployment, id| retained.contains(deployment) && current.as_ref() != Some(id));
        Ok((settlement, state))
    }

    /// Settle the sets against a boot of the deployment `booted` that reached
    /// healthy and was confirmed ([`Self::settlement`]), and release what
    /// nothing names any more.
    ///
    /// The caller holds the transaction lock and, when
    /// [`Self::sets_need_settling`] says so, a writable SYSTEM.
    pub fn settle_sets(
        &self,
        booted: &str,
        boot: &CoreBoot,
        keys: &[[u8; 32]],
    ) -> Result<SetSettlement> {
        let kind = SetKind::Core;
        let held = set_state::read_state(&self.system, kind)?;
        let (settlement, state) = self.settlement(booted, boot)?;
        if state == held {
            return Ok(settlement);
        }
        // One write moves the sets and the bindings together, so an
        // interruption never leaves a deployment on a set it did not run.
        set_state::write_state(&self.system, kind, &state)?;
        if settlement != SetSettlement::Unchanged {
            set_state::end_trial(&self.meta)?;
        }
        let named = named_sets(&state);
        for id in named_sets(&held).difference(&named) {
            let path = set_state::set_path(&self.system, kind, id);
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
        for id in &named_sets(&state) {
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
        let mut removed = 0;
        let held = cores.symlink_metadata().is_ok();
        for entry in held
            .then(|| std::fs::read_dir(&cores))
            .transpose()?
            .into_iter()
            .flatten()
        {
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
        if held {
            sync_dir(&cores)?;
        }
        if let Some(ubi) = self.ubi() {
            for volume in ubi.volumes()? {
                if volume
                    .name
                    .strip_prefix("c-")
                    .is_some_and(|id| valid_id(id).is_ok() && !kept.contains(id))
                {
                    ubi.remove(volume.id)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

/// Every set the state names.
fn named_sets(state: &SetState) -> BTreeSet<String> {
    [&state.current, &state.pending]
        .into_iter()
        .flatten()
        .chain(state.bound.values())
        .cloned()
        .collect()
}
