//! The `core` kind of set: which of a set's components a device composes, and
//! what the runkit composes over the root at boot.
//!
//! A set carries every component of its channel. Which of them a device
//! composes is its product's choice ([`select`]): the one rule, applied by the
//! runkit before it maps anything, by acquisition before it fetches anything,
//! and by the producer before it publishes a set.
use std::path::Path;

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use crate::components::{ContractError, CoreComponent, name, needs_met, require};
use crate::set_state::{component_dir, load_set, read_state, spend_attempt};
use crate::update_set::{SetKind, UpdateSet};

type Result<T> = std::result::Result<T, ContractError>;

/// The components a device composes from `set`: those a feature of its product (`features`, from
/// its product file) selects. Each runs on its root's interface `level`, and every need of a
/// selected component is itself selected, at a version in range. The rule mica-build applies before
/// it publishes a set, so a set no product of the channel can boot is never offered.
pub fn select<'a>(
    set: &'a UpdateSet,
    features: &[String],
    level: u64,
) -> Result<Vec<&'a CoreComponent>> {
    let chosen: Vec<&CoreComponent> = set
        .components
        .iter()
        .filter(|core| core.features.iter().any(|f| features.contains(f)))
        .collect();
    for core in &chosen {
        require(
            core.runs_on(level),
            "a selected core component does not run on this root's interface level",
        )?;
        needs_met(
            core,
            chosen.iter().copied(),
            "a selected core component's need is not selected",
        )?;
    }
    Ok(chosen)
}

/// The features of `/usr/lib/mica/product.conf`: its one `FEATURES="<f> ..."` line.
pub fn device_features(text: &str) -> Result<Vec<String>> {
    let mut found = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("FEATURES=") {
            require(
                found.is_none(),
                "product file names its features more than once",
            )?;
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or(ContractError("invalid features in the product file"))?;
            let features: Vec<String> = value.split_ascii_whitespace().map(str::to_owned).collect();
            for feature in &features {
                name(feature).map_err(|_| ContractError("invalid features in the product file"))?;
            }
            found = Some(features);
        }
    }
    found.ok_or(ContractError("product file names no features"))
}

/// What the runkit composed, left in `/run/mica/core.json` for the running
/// system: the health gate's confirmation commits or drops by it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CoreBoot {
    /// The set composed.
    pub core_set_id: String,
    /// Whether that set is the one on trial.
    #[serde(default)]
    pub pending: bool,
}

/// Where the runkit leaves [`CoreBoot`].
pub const BOOT_FILE: &str = "/run/mica/core.json";

/// What the runkit composes over the root this boot: a core set, already
/// authenticated, known to select on this root, with every component there.
#[derive(Debug)]
pub struct Composition {
    pub id: String,
    pub set: UpdateSet,
    pub envelope: Vec<u8>,
    /// Whether it is the set on trial. A boot that fails with it is the set's
    /// failure, never the deployment's.
    pub pending: bool,
}

/// The root a boot composes over, as far as the choice of core components
/// depends on it.
#[derive(Debug, Clone, Copy)]
pub struct BootRoot<'a> {
    pub arch: &'a str,
    /// The features of the root's product file.
    pub features: &'a [String],
    /// The root's interface level.
    pub level: u64,
}

/// Decide what to compose, before anything of a core component is mapped.
///
/// The pending set is tried when its trial has a boot left, which is spent
/// first. A pending set that does not authenticate, is of another
/// architecture or does not select on this root is passed over, with the
/// reason given to `report`, and the current set is used. A current set that
/// fails the same way is an error: there is nothing else to compose.
pub fn composition(
    system: &Path,
    meta: &Path,
    keys: &[[u8; 32]],
    root: &BootRoot<'_>,
    mut report: impl FnMut(&str),
) -> anyhow::Result<Composition> {
    let BootRoot {
        arch,
        features,
        level,
    } = *root;
    let state = read_state(system, SetKind::Core)?;
    let usable = |id: &str| -> anyhow::Result<(UpdateSet, Vec<u8>)> {
        let (set, envelope) = load_set(system, SetKind::Core, id, keys, arch)?;
        // Every object of what would be composed is there, at its signed
        // length: a set with a component missing is found out here, where
        // there is still something else to compose, not while mapping it.
        for core in select(&set, features, level)? {
            let directory = component_dir(system, SetKind::Core, &core.id);
            for (file, artifact) in [
                ("core.img", &core.content.image),
                ("core.roothash.p7s", &core.content.signature),
            ] {
                let path = directory.join(file);
                let held = path
                    .symlink_metadata()
                    .with_context(|| format!("core component {} {file}", core.package))?;
                ensure!(
                    held.is_file() && held.len() == artifact.bytes,
                    "core component {} {file} is not the signed length",
                    core.package
                );
            }
        }
        Ok((set, envelope))
    };
    if let Some(pending) = &state.pending
        && spend_attempt(meta, SetKind::Core, pending)
    {
        match usable(pending) {
            Ok((set, envelope)) => {
                return Ok(Composition {
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
    // Every image carries a seed set, so a device with none has lost its
    // state, and there is nothing a deployment could stand in with.
    let current = state.current.context("this device holds no core set")?;
    let (set, envelope) = usable(&current).with_context(|| format!("core set {current}"))?;
    Ok(Composition {
        id: current,
        set,
        envelope,
        pending: false,
    })
}
