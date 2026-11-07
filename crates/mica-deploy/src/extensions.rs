//! Driver extensions on SYSTEM (`docs/mica-core.md` 6.1 and 6.5): which build
//! of each extension runs on which kernel, the objects, and what a boot
//! makes of them.
//!
//! SYSTEM `extensions/state.json` names, per kernel component id, the build
//! of each extension by its extension id (`driver_ext`). Each build is in
//! `extensions/objects/<extId>/`: its descriptor `ext.json`, its root hash
//! signature `ext.roothash.p7s` and its image `ext.img`, which on a `ubi`
//! board is the static volume `x-<extId>` instead. The state is written
//! after the objects it names, so a build is whole before it counts; objects
//! nothing names are collected.
//!
//! The runkit maps the builds the state names for the booted kernel, after
//! the owner keyring is sealed (`boot::trust`), holds each mapped image's
//! manifest to its descriptor and kernel and its trees to its manifest, and
//! records what it did in [`BOOT_FILE`].

use crate::driver_ext::Manifest;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// The extensions' directory on SYSTEM.
pub const DIR: &str = "extensions";
/// The state, in [`DIR`].
const STATE: &str = "state.json";
/// The builds' objects, in [`DIR`].
const OBJECTS: &str = "objects";
/// A build's image volume on a `ubi` board: `x-<extId>`.
pub const VOLUME_PREFIX: &str = "x-";
/// The most extensions one kernel runs.
pub const MAX_EXTENSIONS: usize = 16;
/// The most kernels the state names builds for.
const MAX_KERNELS: usize = 8;
/// What the runkit did with the extensions at this boot: [`BootExtensions`].
pub const BOOT_FILE: &str = "/run/mica/extensions.json";
/// The kernel feature word of a kernel whose runkit maps extensions.
pub const KERNEL_FEATURE: &str = "driver-ext";
/// Where the runkit mounts each mapped extension: `<MOUNTS>/<name>`.
pub const MOUNTS: &str = "/run/mica-ext";
/// The runkit's tmpfs for the composed modules' index.
pub const MODULE_INDEX: &str = "/run/mica-modules";
/// The extensions' modules over the support image's, without an index,
/// where `depmod` reads them (as `<base>/usr/lib/modules`).
pub const MODULE_STAGING: &str = "/run/mica-modules/base/usr/lib/modules";
/// The index `depmod` writes for them (as `<out>/usr/lib/modules`).
pub const MODULE_INDEX_LAYER: &str = "/run/mica-modules/out/usr/lib/modules";

/// `state.json`: kernel component id → extension name → extension id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub builds: BTreeMap<String, BTreeMap<String, String>>,
}

impl State {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.builds.len() <= MAX_KERNELS,
            "too many extension kernels"
        );
        for (kernel, builds) in &self.builds {
            crate::deployments::valid_id(kernel)?;
            ensure!(
                !builds.is_empty() && builds.len() <= MAX_EXTENSIONS,
                "invalid extension builds"
            );
            for (name, id) in builds {
                ensure!(valid_name(name), "invalid extension name");
                crate::deployments::valid_id(id)?;
            }
        }
        Ok(())
    }

    /// Every extension id the state names.
    #[must_use]
    pub fn ids(&self) -> BTreeSet<&str> {
        self.builds
            .values()
            .flat_map(|builds| builds.values().map(String::as_str))
            .collect()
    }
}

/// An extension name as `driver_ext` admits it.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[must_use]
pub fn dir(system: &Path) -> PathBuf {
    system.join(DIR)
}

#[must_use]
pub fn objects_dir(system: &Path) -> PathBuf {
    dir(system).join(OBJECTS)
}

/// A build's object directory.
#[must_use]
pub fn object_dir(system: &Path, id: &str) -> PathBuf {
    objects_dir(system).join(id)
}

/// The state on the SYSTEM mounted at `system`; none is empty.
///
/// # Errors
///
/// A state that exists and cannot be read or is invalid.
pub fn read_state(system: &Path) -> Result<State> {
    let path = dir(system).join(STATE);
    if path.symlink_metadata().is_err() {
        return Ok(State::default());
    }
    let state: State = serde_json::from_slice(&mica_fs::read_bounded(&path, 65536)?)
        .context("invalid extension state")?;
    state.validate()?;
    Ok(state)
}

/// Replace the state; an empty one removes the file.
///
/// # Errors
///
/// An invalid state, or a write that fails.
pub fn write_state(system: &Path, state: &State) -> Result<()> {
    state.validate()?;
    let directory = dir(system);
    let path = directory.join(STATE);
    if state.builds.is_empty() {
        if path.symlink_metadata().is_ok() {
            fs::remove_file(&path)?;
            mica_fs::sync_dir(&directory)?;
        }
        return Ok(());
    }
    crate::deployments::directory(&directory)?;
    crate::deployments::atomic_write(&path, &serde_json::to_vec(state)?)
}

/// What became of an extension at this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BootState {
    /// Mapped, and its modules and firmware are in the root's trees.
    Mapped,
    /// Mapped but left out of the root's trees; `reason` says why. Its
    /// mapping is released at shutdown.
    Unused,
    /// Not mapped; `reason` says why.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BootExtension {
    pub name: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub state: BootState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// [`BOOT_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BootExtensions {
    /// Why no extension was mapped at all, when none was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    pub extensions: Vec<BootExtension>,
}

impl BootExtensions {
    /// The ids this boot mapped, which a collection keeps.
    #[must_use]
    pub fn mapped(&self) -> BTreeSet<String> {
        self.extensions
            .iter()
            .filter(|e| e.state != BootState::Refused)
            .map(|e| e.id.clone())
            .collect()
    }
}

/// What the runkit recorded at `path`, if anything.
///
/// # Errors
///
/// A record that exists and cannot be read.
pub fn booted(path: &Path) -> Result<Option<BootExtensions>> {
    if path.symlink_metadata().is_err() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&mica_fs::read_bounded(
        path, 262_144,
    )?)?))
}

/// Every regular file below `root`, relative to it, refusing anything but
/// directories and regular files, more than `limit` entries, or depth
/// beyond 8.
fn files(root: &Path, limit: usize) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new(), 0)];
    let mut seen = 0;
    while let Some((directory, prefix, depth)) = pending.pop() {
        ensure!(depth <= 8, "extension tree too deep");
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            seen += 1;
            ensure!(seen <= limit, "too many entries in the extension");
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid extension file name"))?;
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((entry.path(), relative, depth + 1));
            } else if kind.is_file() {
                found.insert(relative);
            } else {
                bail!("the extension holds something not a file: {relative}");
            }
        }
    }
    Ok(found)
}

/// Hold the image mounted at `mount` to `manifest`: `manifest.json`, the
/// modules it lists in the extension's module directory and the firmware it
/// lists, and nothing else.
///
/// # Errors
///
/// Anything missing, anything more, or anything not a file.
pub fn check_tree(mount: &Path, manifest: &Manifest) -> Result<()> {
    let modules = manifest.module_directory();
    let mut expected: BTreeSet<String> = manifest
        .modules
        .iter()
        .map(|path| format!("{modules}/{path}"))
        .chain(
            manifest
                .firmware
                .iter()
                .map(|path| format!("firmware/{path}")),
        )
        .collect();
    expected.insert("manifest.json".to_owned());
    let found = files(mount, expected.len() + 64 + crate::driver_ext::MAX_FIRMWARE)?;
    if let Some(extra) = found.difference(&expected).next() {
        bail!("the extension holds what its manifest does not list: {extra}");
    }
    if let Some(missing) = expected.difference(&found).next() {
        bail!("the extension lacks what its manifest lists: {missing}");
    }
    Ok(())
}

/// What the extensions mapped so far at this boot carry: a later one, in
/// name order, may not carry a module of the same name or a firmware file at
/// the same path.
#[derive(Debug, Default)]
pub struct Claims {
    modules: BTreeSet<String>,
    firmware: BTreeSet<String>,
}

impl Claims {
    /// Claim what `manifest` carries, or claim nothing.
    ///
    /// # Errors
    ///
    /// An earlier extension carries one of its modules or firmware files.
    pub fn claim(&mut self, manifest: &Manifest) -> Result<()> {
        let modules: BTreeSet<String> = manifest
            .modules
            .iter()
            .map(|path| crate::driver_ext::module_name(path))
            .collect();
        if let Some(module) = modules.intersection(&self.modules).next() {
            bail!("an earlier extension carries the module {module}");
        }
        if let Some(path) = manifest
            .firmware
            .iter()
            .find(|p| self.firmware.contains(*p))
        {
            bail!("an earlier extension carries the firmware {path}");
        }
        self.modules.extend(modules);
        self.firmware.extend(manifest.firmware.iter().cloned());
        Ok(())
    }
}

/// `/run/modprobe.d/mica-ext-<name>.conf`: the manifest's module options.
#[must_use]
pub fn modprobe_conf(manifest: &Manifest) -> String {
    manifest
        .options
        .iter()
        .map(|(module, line)| format!("options {module} {line}\n"))
        .collect()
}

/// `/run/modules-load.d/mica-ext-<name>.conf`: the modules to load at boot.
#[must_use]
pub fn modules_load(manifest: &Manifest) -> String {
    manifest
        .load
        .iter()
        .map(|module| format!("{module}\n"))
        .collect()
}

#[cfg(test)]
mod tests;
