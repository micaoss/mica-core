//! Which core set a device composes, and the trial of a new one.
//!
//! Two files, because two parties write and the runkit may write only one of
//! them:
//!
//! - **SYSTEM `core-sets/state.json`** names the `current` set and at most one
//!   `pending` set. Only the running system writes it, through mica-deploy;
//!   the runkit mounts SYSTEM read-only and reads it. It survives a reset of
//!   DATA, so a device always knows which set is its own.
//! - **DATA `meta/core-trial.json`** counts the boots a pending set has left.
//!   The runkit spends one before it tries the set, so a boot that never
//!   reaches the health gate, a watchdog reset included, still costs one. A
//!   trial that is missing, names another set or has nothing left means the
//!   pending set is not tried, and `current` is.
//!
//! A device holds two sets at most. A pending set that reaches healthy becomes
//! `current` and the old one is released; one that runs out of boots is
//! dropped. With no `current` the device composes the core components its
//! `mica/deployment/v1` names, as it did before core sets.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::core_set::{self, CoreSet};
use crate::deployments::{atomic_write, read_bounded, valid_id};

/// The directory of signed sets on SYSTEM, one `<id>.json` each.
pub const SETS_DIR: &str = "core-sets";
/// The state file, inside [`SETS_DIR`].
const STATE_FILE: &str = "core-sets/state.json";
/// The trial file, inside DATA's metadata directory.
const TRIAL_FILE: &str = "core-trial.json";
/// The boots a new set is given to reach healthy, as a new deployment is.
pub const TRIAL_ATTEMPTS: u8 = 3;
/// A signed set's envelope: the payload in base64, a key id and a signature.
pub const MAX_ENVELOPE_BYTES: u64 = 49152;

/// The sets a device holds.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreState {
    /// The set the device composes. Absent on a device that has never taken
    /// one: it composes its deployment's own core components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The set on trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
}

/// The boots the pending set has left.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CoreTrial {
    pub id: String,
    pub attempts_left: u8,
}

/// What the runkit composed, left in `/run/mica/core.json` for the running
/// system: the health gate's confirmation commits or drops by it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CoreBoot {
    /// The set composed; absent when the deployment's own components were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_set_id: Option<String>,
    /// Whether that set is the one on trial.
    #[serde(default)]
    pub pending: bool,
}

/// Where the runkit leaves [`CoreBoot`].
pub const BOOT_FILE: &str = "/run/mica/core.json";

/// Where a set's signed envelope is on SYSTEM.
#[must_use]
pub fn set_path(system: &Path, id: &str) -> PathBuf {
    system.join(SETS_DIR).join(format!("{id}.json"))
}

/// The state on `system`; a SYSTEM that never held a set has none.
pub fn read_state(system: &Path) -> Result<CoreState> {
    let path = system.join(STATE_FILE);
    if path.symlink_metadata().is_err() {
        return Ok(CoreState::default());
    }
    let state: CoreState =
        serde_json::from_slice(&read_bounded(&path, 4096)?).context("invalid core set state")?;
    for id in [&state.current, &state.pending].into_iter().flatten() {
        valid_id(id)?;
    }
    ensure!(
        state.pending.is_none() || state.pending != state.current,
        "the pending core set is the current one"
    );
    Ok(state)
}

/// Write the state; the caller holds SYSTEM writable.
pub fn write_state(system: &Path, state: &CoreState) -> Result<()> {
    atomic_write(&system.join(STATE_FILE), &serde_json::to_vec(state)?)
}

/// The trial in `meta`, or none: a file that is absent, unreadable or
/// malformed is no trial, which is the answer that tries nothing new.
#[must_use]
pub fn read_trial(meta: &Path) -> Option<CoreTrial> {
    let bytes = read_bounded(&meta.join(TRIAL_FILE), 4096).ok()?;
    let trial: CoreTrial = serde_json::from_slice(&bytes).ok()?;
    valid_id(&trial.id).ok()?;
    Some(trial)
}

/// Give `id` its boots.
pub fn start_trial(meta: &Path, id: &str) -> Result<()> {
    let trial = CoreTrial {
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
pub fn spend_attempt(meta: &Path, pending: &str) -> bool {
    let Some(trial) = read_trial(meta) else {
        return false;
    };
    if trial.id != pending || trial.attempts_left == 0 {
        return false;
    }
    let spent = CoreTrial {
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
pub fn trial_is_over(meta: &Path, pending: &str) -> bool {
    read_trial(meta).is_none_or(|trial| trial.id != pending || trial.attempts_left == 0)
}

/// The signed set `id` on `system`, authenticated, of `arch`, with its
/// envelope's bytes.
pub fn load_set(
    system: &Path,
    id: &str,
    keys: &[[u8; 32]],
    arch: &str,
) -> Result<(CoreSet, Vec<u8>)> {
    valid_id(id)?;
    let envelope = read_bounded(&set_path(system, id), MAX_ENVELOPE_BYTES)
        .with_context(|| format!("read core set {id}"))?;
    let set = core_set::authenticate(&envelope, keys)?;
    ensure!(set.id()? == id, "core set identity mismatch");
    ensure!(set.arch == arch, "core set is of another architecture");
    Ok((set, envelope))
}

/// What the runkit composes over the root this boot.
#[derive(Debug)]
pub enum Composition {
    /// The core components the deployment itself names (`mica/deployment/v1`),
    /// or none: the device has never taken a core set.
    Deployment,
    /// A core set, already authenticated and known to select on this root.
    Set {
        id: String,
        set: CoreSet,
        envelope: Vec<u8>,
        /// Whether it is the set on trial. A boot that fails with it is the
        /// set's failure, never the deployment's.
        pending: bool,
    },
}

/// Decide what to compose, before anything of a core component is mapped.
///
/// The pending set is tried when its trial has a boot left, which is spent
/// first. A pending set that does not authenticate, is of another
/// architecture or does not select on this root is passed over, with the
/// reason given to `report`, and the current set is used. A current set that
/// fails the same way is an error: there is nothing older to go back to.
pub fn composition(
    system: &Path,
    meta: &Path,
    keys: &[[u8; 32]],
    arch: &str,
    features: &[String],
    level: u64,
    mut report: impl FnMut(&str),
) -> Result<Composition> {
    let state = read_state(system)?;
    let usable = |id: &str| -> Result<(CoreSet, Vec<u8>)> {
        let (set, envelope) = load_set(system, id, keys, arch)?;
        core_set::select(&set, features, level)?;
        Ok((set, envelope))
    };
    if let Some(pending) = &state.pending
        && spend_attempt(meta, pending)
    {
        match usable(pending) {
            Ok((set, envelope)) => {
                return Ok(Composition::Set {
                    id: pending.clone(),
                    set,
                    envelope,
                    pending: true,
                });
            }
            Err(error) => report(&format!(
                "pending core set {pending} refused: {error:#}; composing the current one"
            )),
        }
    }
    let Some(current) = state.current else {
        return Ok(Composition::Deployment);
    };
    let (set, envelope) = usable(&current).with_context(|| format!("core set {current}"))?;
    Ok(Composition::Set {
        id: current,
        set,
        envelope,
        pending: false,
    })
}

#[cfg(test)]
mod tests;
