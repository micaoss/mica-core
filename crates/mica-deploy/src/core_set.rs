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

use crate::components::{ContractError, CoreComponent, RootFacts, name, needs_met, require};
use crate::set_state::{component_dir, load_set, read_state, spend_attempt};
use crate::update_set::{SetKind, UpdateSet};

type Result<T> = std::result::Result<T, ContractError>;

/// Why a set's selection does not run on a root: the rule, the component and
/// the constraint that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionError {
    pub reason: &'static str,
    pub package: String,
    pub constraint: String,
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} ({})", self.reason, self.package, self.constraint)
    }
}

impl std::error::Error for SelectionError {}

/// The components a device composes from `set`: those a feature of its product (`features`, from
/// its product file) selects. Each is held to what it declares of the root (`root`, its interface
/// levels; `requires`, root packages), and every need of a selected component is itself
/// selected, at a version in range. What a component does not declare is not checked.
pub fn select<'a>(
    set: &'a UpdateSet,
    features: &[String],
    root: &RootFacts<'_>,
) -> std::result::Result<Vec<&'a CoreComponent>, SelectionError> {
    let chosen: Vec<&CoreComponent> = set
        .components
        .iter()
        .filter(|core| core.features.iter().any(|f| features.contains(f)))
        .collect();
    for core in &chosen {
        let refused = |reason, constraint: String| SelectionError {
            reason,
            package: core.package.clone(),
            constraint,
        };
        if !core.runs_on(root.level) {
            let range = core
                .root
                .as_ref()
                .map_or_else(String::new, |range| match range.max {
                    Some(max) => format!("{}..{max}", range.min),
                    None => format!("{}..", range.min),
                });
            return Err(refused(
                "a selected core component does not run on this root's interface level",
                format!("root {range}, the root is level {}", root.level),
            ));
        }
        if let Some(requirement) = core.requires.iter().find(|r| !r.holds(root)) {
            let provided = root
                .provides
                .and_then(|provides| provides.get(&requirement.package))
                .map_or("", String::as_str);
            return Err(refused(
                "a selected core component's root requirement does not hold",
                format!("{}, the root provides {provided}", requirement.describe()),
            ));
        }
        needs_met(
            core,
            chosen.iter().copied(),
            "a selected core component's need is not selected",
        )
        .map_err(|error| {
            refused(
                error.0,
                format!(
                    "needs {:?}",
                    core.needs
                        .iter()
                        .map(|n| n.package.as_str())
                        .collect::<Vec<_>>()
                ),
            )
        })?;
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
    /// Whether the set on trial is paired with the deployment booted: the
    /// trial is the deployment's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paired: bool,
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
    /// Whether it is the set on trial. A boot that fails with a set alone on
    /// trial is the set's failure, never the deployment's.
    pub pending: bool,
    /// Whether the set on trial is paired with the deployment booting, whose
    /// trial it shares.
    pub paired: bool,
}

/// The root a boot composes over, as far as the choice of core components
/// depends on it.
#[derive(Debug, Clone, Copy)]
pub struct BootRoot<'a> {
    pub arch: &'a str,
    /// The features of the root's product file.
    pub features: &'a [String],
    /// What the root declares.
    pub root: RootFacts<'a>,
}

/// Decide what to compose for the booting `deployment`, before anything of a
/// core component is mapped.
///
/// A pending set paired with `deployment` is composed, and a set bound to it.
/// Otherwise a pending set alone is tried when its trial has a boot left,
/// which is spent first; one that does not authenticate, is of another
/// architecture or does not select on this root is passed over, with the
/// reason given to `report`, and the current set is used. A set that must be
/// composed and fails the same way is an error: there is nothing else to
/// compose. On a `ubi` board, `ubi` holds the components' images as static
/// volumes.
pub fn composition(
    system: &Path,
    meta: &Path,
    ubi: Option<&dyn crate::ubi::UbiDevice>,
    keys: &[[u8; 32]],
    root: &BootRoot<'_>,
    deployment: &str,
    mut report: impl FnMut(&str),
) -> anyhow::Result<Composition> {
    let BootRoot {
        arch,
        features,
        root: facts,
    } = *root;
    let state = read_state(system, SetKind::Core)?;
    let usable = |id: &str| -> anyhow::Result<(UpdateSet, Vec<u8>)> {
        let (set, envelope) = load_set(system, SetKind::Core, id, keys, arch)?;
        // Every object of what would be composed is there, at its signed
        // length: a set with a component missing is found out here, where
        // there is still something else to compose, not while mapping it.
        for core in select(&set, features, &facts)? {
            let directory = component_dir(system, SetKind::Core, &core.id);
            if let Some(ubi) = ubi {
                let name = format!("c-{}", core.id);
                let volume = crate::ubi::find(ubi, &name)?
                    .with_context(|| format!("core component {} core.img", core.package))?;
                ensure!(
                    volume.kind == crate::ubi::VolumeKind::Static
                        && volume.sound()
                        && volume.data_bytes == core.content.image.bytes,
                    "core component {} core.img is not the signed length",
                    core.package
                );
            }
            for (file, artifact) in [
                ("core.img", &core.content.image),
                ("core.roothash.p7s", &core.content.signature),
            ] {
                if ubi.is_some() && file == "core.img" {
                    continue;
                }
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
    let paired = state.paired();
    if let Some(pending) = &state.pending
        && paired == Some(deployment)
    {
        let (set, envelope) = usable(pending)
            .with_context(|| format!("core set {pending}, paired with this deployment"))?;
        return Ok(Composition {
            id: pending.clone(),
            set,
            envelope,
            pending: true,
            paired: true,
        });
    }
    if let Some(bound) = state.bound.get(deployment) {
        let (set, envelope) =
            usable(bound).with_context(|| format!("core set {bound}, bound to this deployment"))?;
        return Ok(Composition {
            id: bound.clone(),
            set,
            envelope,
            pending: false,
            paired: false,
        });
    }
    if let Some(pending) = &state.pending
        && paired.is_none()
        && spend_attempt(meta, SetKind::Core, pending)
    {
        match usable(pending) {
            Ok((set, envelope)) => {
                return Ok(Composition {
                    id: pending.clone(),
                    set,
                    envelope,
                    pending: true,
                    paired: false,
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
        paired: false,
    })
}
