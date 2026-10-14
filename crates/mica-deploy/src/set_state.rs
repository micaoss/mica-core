//! Which set of a kind a device holds, and the trial of a new one.
//!
//! Two files, because two parties write and the runkit may write only one of
//! them:
//!
//! - **SYSTEM `sets/<kind>/state.json`** names the `current` set of the kind
//!   and at most one `pending` set. Only the running system writes it, through
//!   mica-deploy; the runkit mounts SYSTEM read-only and reads it. It survives
//!   a reset of DATA, so a device always knows which set is its own.
//! - **DATA `meta/set-trial.json`** counts the boots a pending set has left.
//!   There is one, for every kind: one thing is on trial at a time, and the
//!   file's shape says so. The runkit spends a boot before it tries the set,
//!   so a boot that never reaches the health gate, a watchdog reset included,
//!   still costs one. A trial that is missing, names another set or has
//!   nothing left means the pending set is not tried, and `current` is.
//!
//! A device holds two sets of a kind at most. A pending set that reaches
//! healthy becomes `current` and the old one is released; one that runs out of
//! boots is dropped. Every image carries a seed core set as its first
//! `current`: a deployment names no core components, so a device with no core
//! set has nothing to compose.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::deployments::{atomic_write, read_bounded, valid_id};
use crate::update_set::{self, SetKind, UpdateSet};

/// The directory of every kind's sets on SYSTEM.
pub const SETS_DIR: &str = "sets";
/// The trial file, inside DATA's metadata directory.
const TRIAL_FILE: &str = "set-trial.json";
/// The boots a new set is given to reach healthy, as a new deployment is.
pub const TRIAL_ATTEMPTS: u8 = 3;
/// A signed set's envelope: the payload in base64, a key id and a signature.
pub const MAX_ENVELOPE_BYTES: u64 = 49152;

/// The sets of one kind a device holds.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetState {
    /// The set in effect. For core, absent only before the first set is
    /// installed, which on a device is never: its image carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The set on trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
}

/// The boots the pending set has left.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SetTrial {
    pub kind: SetKind,
    pub id: String,
    pub attempts_left: u8,
}

/// The directory of one kind's sets on `system`: `<id>.json` each, the state
/// and the objects.
#[must_use]
pub fn kind_dir(system: &Path, kind: SetKind) -> PathBuf {
    system.join(SETS_DIR).join(kind.as_str())
}

/// The directory of a kind's component objects. For core,
/// `<component id>/core.img`, `core.roothash.p7s` and `core.roothash`.
///
/// Beside the sets that name them, and collected with them: installation and
/// collection of a deployment never look in here.
#[must_use]
pub fn objects_dir(system: &Path, kind: SetKind) -> PathBuf {
    kind_dir(system, kind).join("objects")
}

/// The directory of one of a set's components on `system`.
#[must_use]
pub fn component_dir(system: &Path, kind: SetKind, component: &str) -> PathBuf {
    objects_dir(system, kind).join(component)
}

/// Where a set's signed envelope is on SYSTEM.
#[must_use]
pub fn set_path(system: &Path, kind: SetKind, id: &str) -> PathBuf {
    kind_dir(system, kind).join(format!("{id}.json"))
}

fn state_path(system: &Path, kind: SetKind) -> PathBuf {
    kind_dir(system, kind).join("state.json")
}

/// The state of `kind` on `system`; a SYSTEM that never held a set of it has
/// none.
pub fn read_state(system: &Path, kind: SetKind) -> Result<SetState> {
    let path = state_path(system, kind);
    if path.symlink_metadata().is_err() {
        return Ok(SetState::default());
    }
    let state: SetState =
        serde_json::from_slice(&read_bounded(&path, 4096)?).context("invalid set state")?;
    for id in [&state.current, &state.pending].into_iter().flatten() {
        valid_id(id)?;
    }
    ensure!(
        state.pending.is_none() || state.pending != state.current,
        "the pending set is the current one"
    );
    Ok(state)
}

/// Write the state; the caller holds SYSTEM writable.
pub fn write_state(system: &Path, kind: SetKind, state: &SetState) -> Result<()> {
    atomic_write(&state_path(system, kind), &serde_json::to_vec(state)?)
}

/// The trial in `meta`, or none: a file that is absent, unreadable, malformed
/// or of a kind this reader does not implement is no trial, which is the
/// answer that tries nothing new.
#[must_use]
pub fn read_trial(meta: &Path) -> Option<SetTrial> {
    let bytes = read_bounded(&meta.join(TRIAL_FILE), 4096).ok()?;
    let trial: SetTrial = serde_json::from_slice(&bytes).ok()?;
    valid_id(&trial.id).ok()?;
    Some(trial)
}

/// Give the set `id` of `kind` its boots.
pub fn start_trial(meta: &Path, kind: SetKind, id: &str) -> Result<()> {
    let trial = SetTrial {
        kind,
        id: id.to_owned(),
        attempts_left: TRIAL_ATTEMPTS,
    };
    atomic_write(&meta.join(TRIAL_FILE), &serde_json::to_vec(&trial)?)
}

/// Forget the trial.
pub fn end_trial(meta: &Path) -> Result<()> {
    let path = meta.join(TRIAL_FILE);
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(&path)?;
        mica_fs::sync_dir(meta)?;
    }
    Ok(())
}

/// The runkit's step before it tries a pending set: spend one boot of its
/// trial, durably, and say whether the set is to be tried. Nothing is tried on
/// a trial that is not this set's or has no boot left, or when the spent count
/// cannot be made durable: a set tried without a count is a set tried forever.
#[must_use]
pub fn spend_attempt(meta: &Path, kind: SetKind, pending: &str) -> bool {
    let Some(trial) = read_trial(meta) else {
        return false;
    };
    if trial.kind != kind || trial.id != pending || trial.attempts_left == 0 {
        return false;
    }
    let spent = SetTrial {
        attempts_left: trial.attempts_left - 1,
        ..trial
    };
    serde_json::to_vec(&spent)
        .ok()
        .is_some_and(|bytes| atomic_write(&meta.join(TRIAL_FILE), &bytes).is_ok())
}

/// Whether a pending set that was not booted is finished: its trial is gone,
/// is another set's, or has no boot left.
#[must_use]
pub fn trial_is_over(meta: &Path, kind: SetKind, pending: &str) -> bool {
    read_trial(meta)
        .is_none_or(|trial| trial.kind != kind || trial.id != pending || trial.attempts_left == 0)
}

/// The signed set `id` of `kind` on `system`, authenticated, of `arch`, with
/// its envelope's bytes.
pub fn load_set(
    system: &Path,
    kind: SetKind,
    id: &str,
    keys: &[[u8; 32]],
    arch: &str,
) -> Result<(UpdateSet, Vec<u8>)> {
    valid_id(id)?;
    let envelope = read_bounded(&set_path(system, kind, id), MAX_ENVELOPE_BYTES)
        .with_context(|| format!("read {kind} set {id}"))?;
    let set = update_set::authenticate(&envelope, keys)?;
    ensure!(set.id()? == id, "set identity mismatch");
    ensure!(set.kind == kind, "set is of another kind");
    ensure!(set.arch == arch, "set is of another architecture");
    Ok((set, envelope))
}

#[cfg(test)]
mod tests;
