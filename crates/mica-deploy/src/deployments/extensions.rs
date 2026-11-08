//! Installing, removing, trying and collecting driver extensions on SYSTEM
//! (`docs/mica-core.md` 6.3; the layout is `crate::extensions`'s).

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use super::*;
use crate::components::{Deployment, KernelComponent};
use crate::driver_ext::Extension;
use crate::extensions::{self as ext, BootTrial, Builds, MAX_EXTENSIONS, VOLUME_PREFIX};
use crate::set_state::{self, TrialKind};
use crate::update_set::SetKind;

/// An extension the running kernel runs and a new kernel has no build of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wait {
    pub name: String,
    pub kernel: String,
}

/// Why a system update is refused: its kernel has no build of extensions
/// the running kernel runs (`waitsForExtension`).
#[derive(Debug)]
pub struct WaitsForExtension(pub Vec<Wait>);

impl std::fmt::Display for WaitsForExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.0.iter().map(|wait| wait.name.as_str()).collect();
        write!(
            f,
            "waitsForExtension: the new kernel has no build of {}; give one with --extension, or leave it out with --drop-extension",
            names.join(", ")
        )
    }
}

impl std::error::Error for WaitsForExtension {}

/// The driver extensions a system update brings for its kernel.
#[derive(Debug, Default)]
pub struct KernelExtensions {
    /// Descriptors of builds for the new kernel; their objects are in the
    /// update's objects, by digest, or else in `staged`.
    pub descriptors: Vec<Vec<u8>>,
    /// Extensions the new kernel runs without.
    pub dropped: Vec<String>,
    /// Where the objects of builds an archive staged are, by digest: the
    /// acquisition workspace's, whatever objects the update itself names.
    pub staged: Option<PathBuf>,
}

/// The builds a system update pairs with its deployment, checked.
pub(super) struct Pairing {
    builds: Vec<(Vec<u8>, Extension)>,
    pending: Builds,
    staged: Option<PathBuf>,
}

/// Whether `kernel` maps driver extensions of `extension`'s architecture.
fn check_build(kernel: &KernelComponent, extension: &Extension) -> Result<()> {
    ensure!(
        kernel.arch == extension.descriptor.arch,
        "the extension is built for another architecture"
    );
    ensure!(
        kernel
            .features
            .as_ref()
            .is_some_and(|features| features.iter().any(|f| f == ext::KERNEL_FEATURE)),
        "the extension's kernel does not map driver extensions"
    );
    Ok(())
}

impl DeploymentStore {
    /// The kernel component of each deployment the boot records hold, by
    /// deployment id.
    fn retained(&self, keys: &[[u8; 32]]) -> Result<BTreeMap<String, KernelComponent>> {
        let mut kernels = BTreeMap::new();
        for entry in self.entries()? {
            let path = self.system.join(format!("deployments/{}.json", entry.id));
            let deployment =
                crate::components::authenticate_deployment(&read_bounded(&path, 24576)?, keys)?;
            kernels.insert(entry.id, deployment.kernel);
        }
        Ok(kernels)
    }

    /// The kernel components of the deployments the boot records hold, by id.
    fn retained_kernels(&self, keys: &[[u8; 32]]) -> Result<BTreeMap<String, KernelComponent>> {
        Ok(self
            .retained(keys)?
            .into_values()
            .map(|kernel| (kernel.id.clone(), kernel))
            .collect())
    }

    /// The kernel an extension change on the running deployment `booted` is
    /// for, and the deployment it is paired with: with a deployment on
    /// trial, that deployment's kernel, paired with it; else `booted`'s,
    /// alone. A kernel only a fallback runs takes no change, and nothing
    /// changes while a core set or another change is on trial.
    fn change_target(
        &self,
        booted: &str,
        keys: &[[u8; 32]],
    ) -> Result<(KernelComponent, Option<String>)> {
        let mut retained = self.retained(keys)?;
        let candidate = self.effective_state()?.candidate;
        let cores = set_state::read_state(&self.system, SetKind::Core)?;
        ensure!(
            cores.pending.is_none() || cores.paired().is_some() && cores.pair == candidate,
            "a core set is on trial; let it settle before an extension change"
        );
        let state = ext::read_state(&self.system)?;
        ensure!(
            state.pending.is_none() || state.pair == candidate,
            "another extension change is on trial; let it settle first"
        );
        match candidate {
            Some(candidate) => {
                ensure!(
                    candidate != booted,
                    "confirm the running deployment before an extension change"
                );
                let kernel = retained
                    .remove(&candidate)
                    .context("the deployment on trial is not held")?;
                Ok((kernel, Some(candidate)))
            }
            None => Ok((
                retained
                    .remove(booted)
                    .context("the running deployment is not retained")?,
                None,
            )),
        }
    }

    /// Make `pending` the change on trial in `state`: paired with `pair`,
    /// or alone with a trial of its own, started before the change is
    /// named. A change to what is confirmed is no change, and the change
    /// already on trial keeps its trial.
    fn propose(&self, mut state: ext::State, pending: Builds, pair: Option<String>) -> Result<()> {
        if pending == state.builds {
            if state.pending.take().is_some() {
                if state.pair.take().is_none() {
                    set_state::end_trial(&self.meta)?;
                }
                ext::write_state(&self.system, &state)?;
            }
            return Ok(());
        }
        if state.pending.as_ref() == Some(&pending) && state.pair == pair {
            return Ok(());
        }
        state.pending = Some(pending);
        state.pair = pair;
        if state.pair.is_none() {
            // A pending change with no trial is never tried; a trial with no
            // pending change is ignored.
            let id = state.pending_id().context("no pending change")?;
            set_state::start_trial(&self.meta, TrialKind::Extensions, &id)?;
        }
        ext::write_state(&self.system, &state)
    }

    /// Install the driver extension whose descriptor is `descriptor`, with
    /// its image at `image` and its root hash signature at `signature`, as a
    /// change on trial ([`Self::change_target`]) that replaces the build of
    /// the same name for its kernel; answer its extension id. `booted` is
    /// the running deployment. The kernel must say that its runkit maps
    /// extensions. The signature is the kernel's to check, when the image is
    /// mapped at the next boot of that kernel. The caller holds the
    /// transaction lock and writable mounts.
    ///
    /// # Errors
    ///
    /// An invalid descriptor, a kernel that takes no change now or maps no
    /// extensions, objects that are not the descriptor's, too many
    /// extensions, too little space, or a write that fails.
    pub fn install_extension(
        &self,
        descriptor: &[u8],
        image: &Path,
        signature: &Path,
        keys: &[[u8; 32]],
        booted: &str,
    ) -> Result<String> {
        let extension = crate::driver_ext::parse_descriptor(descriptor)?;
        let d = &extension.descriptor;
        let (kernel, pair) = self.change_target(booted, keys)?;
        ensure!(
            d.kernel == kernel.id,
            "extensions change only for the running kernel, or the kernel on trial"
        );
        check_build(&kernel, &extension)?;
        let state = ext::read_state(&self.system)?;
        let mut pending = state
            .pending
            .clone()
            .unwrap_or_else(|| state.builds.clone());
        let builds = pending.entry(d.kernel.clone()).or_default();
        ensure!(
            builds.contains_key(&d.name) || builds.len() < MAX_EXTENSIONS,
            "too many driver extensions for one kernel"
        );
        self.publish_build(descriptor, &extension, image, signature)?;
        builds.insert(d.name.clone(), extension.id.clone());
        self.propose(state, pending, pair)?;
        Ok(extension.id)
    }

    /// Publish a build's objects and descriptor, within the reserve.
    fn publish_build(
        &self,
        descriptor: &[u8],
        extension: &Extension,
        image: &Path,
        signature: &Path,
    ) -> Result<()> {
        let d = &extension.descriptor;
        let object_dir = ext::object_dir(&self.system, &extension.id);
        let files = [
            (
                Place::File(object_dir.join("ext.roothash.p7s")),
                &d.content.signature,
                signature,
            ),
            (
                self.large_object(
                    format!("{}/objects/{}/ext.img", ext::DIR, extension.id),
                    format!("{VOLUME_PREFIX}{}", extension.id),
                ),
                &d.content.image,
                image,
            ),
        ];
        let mut bytes = descriptor.len() as u64;
        let mut lebs = 0;
        for (place, artifact, source) in &files {
            if self.held(place)? {
                self.verify_held(place, artifact)?;
                continue;
            }
            verify_file(source, artifact)?;
            match (place, self.ubi()) {
                (Place::Volume(_), Some(ubi)) => lebs += crate::ubi::lebs(ubi, artifact.bytes),
                _ => bytes += artifact.bytes,
            }
        }
        directory(&object_dir)?;
        if let Some(ubi) = self.ubi() {
            ensure!(
                ubi.available_lebs()? >= lebs + crate::ubi::lebs(ubi, self.reserves.system()),
                "insufficient UBI space"
            );
        } else {
            let space = rustix::fs::statvfs(&self.system)?;
            ensure!(
                space.f_bavail * space.f_frsize >= bytes + self.reserves.system(),
                "insufficient SYSTEM space"
            );
        }
        for (place, artifact, source) in &files {
            self.publish(source, place, artifact)?;
        }
        let path = object_dir.join("ext.json");
        if path.symlink_metadata().is_ok() {
            ensure!(
                read_bounded(&path, 8192)? == descriptor,
                "extension id was reused"
            );
        } else {
            atomic_write(&path, descriptor)?;
        }
        sync_dir(&object_dir)?;
        Ok(())
    }

    /// Stop installing the extension `name` on the kernel a change is for
    /// ([`Self::change_target`]), which `kernel` names when it is given, as a
    /// change on trial; `booted` is the running deployment. Answer how many
    /// builds the change leaves out. The caller holds the transaction lock
    /// and writable mounts.
    ///
    /// # Errors
    ///
    /// A kernel that takes no change now, or a state that cannot be read or
    /// written.
    pub fn remove_extension(
        &self,
        name: &str,
        kernel: Option<&str>,
        keys: &[[u8; 32]],
        booted: &str,
    ) -> Result<usize> {
        let (target, pair) = self.change_target(booted, keys)?;
        ensure!(
            kernel.is_none_or(|kernel| kernel == target.id),
            "extensions change only for the running kernel, or the kernel on trial"
        );
        let state = ext::read_state(&self.system)?;
        let mut pending = state
            .pending
            .clone()
            .unwrap_or_else(|| state.builds.clone());
        let removed = pending
            .get_mut(&target.id)
            .is_some_and(|builds| builds.remove(name).is_some());
        pending.retain(|_, builds| !builds.is_empty());
        if removed {
            self.propose(state, pending, pair)?;
        }
        Ok(usize::from(removed))
    }

    /// Whether confirming a boot of the deployment `booted`, which mapped the
    /// pending change `trial` if any, changes the extension state, and so
    /// needs a writable SYSTEM.
    ///
    /// # Errors
    ///
    /// The state or the retained deployments cannot be read.
    pub fn extensions_need_settling(
        &self,
        booted: &str,
        trial: Option<&BootTrial>,
        keys: &[[u8; 32]],
    ) -> Result<bool> {
        let held = ext::read_state(&self.system)?;
        Ok(self.extension_settlement(booted, trial, keys)?.1 != held)
    }

    /// What a healthy boot of the deployment `booted` means for the pending
    /// change, and the state it leaves. The change the boot mapped (`trial`)
    /// is committed, without kernels no retained deployment runs. A pair
    /// whose deployment is booted without it or is no longer retained, or a
    /// change alone whose trial is over, is dropped.
    fn extension_settlement(
        &self,
        booted: &str,
        trial: Option<&BootTrial>,
        keys: &[[u8; 32]],
    ) -> Result<(SetSettlement, ext::State)> {
        let mut state = ext::read_state(&self.system)?;
        let Some(id) = state.pending_id() else {
            return Ok((SetSettlement::Unchanged, state));
        };
        let settlement = if trial.is_some_and(|t| t.id == id && t.paired == state.pair.is_some()) {
            SetSettlement::Committed
        } else {
            match state.pair.as_deref() {
                Some(pair)
                    if pair != booted
                        && self.retained_deployments(Some(booted))?.contains(pair) =>
                {
                    SetSettlement::Unchanged
                }
                Some(_) => SetSettlement::Dropped,
                None if set_state::trial_is_over(&self.meta, TrialKind::Extensions, &id) => {
                    SetSettlement::Dropped
                }
                None => SetSettlement::Unchanged,
            }
        };
        match settlement {
            SetSettlement::Committed => {
                let kernels = self.retained_kernels(keys)?;
                let mut pending = state.pending.take().unwrap_or_default();
                pending.retain(|kernel, _| kernels.contains_key(kernel));
                state.builds = pending;
                state.pair = None;
            }
            SetSettlement::Dropped => {
                state.pending = None;
                state.pair = None;
            }
            SetSettlement::Unchanged => {}
        }
        Ok((settlement, state))
    }

    /// Settle the pending change against a boot of the deployment `booted`
    /// that reached healthy and was confirmed
    /// ([`Self::extension_settlement`]). Objects nothing names any more are
    /// left to a collection. The caller holds the transaction lock and, when
    /// [`Self::extensions_need_settling`] says so, a writable SYSTEM.
    ///
    /// # Errors
    ///
    /// The state cannot be read or written.
    pub fn settle_extensions(
        &self,
        booted: &str,
        trial: Option<&BootTrial>,
        keys: &[[u8; 32]],
    ) -> Result<SetSettlement> {
        let held = ext::read_state(&self.system)?;
        let (settlement, state) = self.extension_settlement(booted, trial, keys)?;
        if state == held {
            return Ok(settlement);
        }
        ext::write_state(&self.system, &state)?;
        if settlement != SetSettlement::Unchanged
            && set_state::read_trial(&self.meta)
                .is_some_and(|trial| trial.kind == TrialKind::Extensions)
        {
            set_state::end_trial(&self.meta)?;
        }
        Ok(settlement)
    }

    /// The extensions the running kernel `running` runs that the kernel
    /// `kernel` of a system update has no build of.
    ///
    /// # Errors
    ///
    /// The state cannot be read.
    pub fn waits_for_extension(&self, running: &str, kernel: &str) -> Result<Vec<Wait>> {
        let state = ext::read_state(&self.system)?;
        if running == kernel {
            return Ok(Vec::new());
        }
        let held = state.builds.get(kernel);
        Ok(state
            .builds
            .get(running)
            .into_iter()
            .flat_map(BTreeMap::keys)
            .filter(|name| !held.is_some_and(|builds| builds.contains_key(*name)))
            .map(|name| Wait {
                name: name.clone(),
                kernel: kernel.to_owned(),
            })
            .collect())
    }

    /// Check what a system update of `deployment` brings for its kernel
    /// (`given`), and answer the change it pairs with the deployment, if
    /// any. The update waits while an extension the running kernel
    /// (`running`) runs has no build for the new kernel and is not dropped.
    pub(super) fn pair_extensions(
        &self,
        deployment: &Deployment,
        running: &str,
        given: &KernelExtensions,
    ) -> Result<Option<Pairing>> {
        let state = ext::read_state(&self.system)?;
        // A pair whose deployment never appeared is replaced.
        let retained = self.retained_deployments(None)?;
        ensure!(
            state.pending.is_none() || state.pair.as_ref().is_some_and(|p| !retained.contains(p)),
            "an extension change is on trial; let it settle before a system update"
        );
        let kernel = &deployment.kernel;
        let mut builds = Vec::new();
        let mut names = BTreeSet::new();
        for descriptor in &given.descriptors {
            let extension = crate::driver_ext::parse_descriptor(descriptor)?;
            ensure!(
                extension.descriptor.kernel == kernel.id,
                "an extension given with a system update is built for another kernel"
            );
            check_build(kernel, &extension)?;
            ensure!(
                names.insert(extension.descriptor.name.clone()),
                "an extension is given twice"
            );
            builds.push((descriptor.clone(), extension));
        }
        for name in &given.dropped {
            ensure!(
                ext::valid_name(name) && !names.contains(name),
                "invalid dropped extension {name:?}"
            );
        }
        let waits: Vec<Wait> = self
            .waits_for_extension(running, &kernel.id)?
            .into_iter()
            .filter(|wait| !names.contains(&wait.name) && !given.dropped.contains(&wait.name))
            .collect();
        if !waits.is_empty() {
            return Err(WaitsForExtension(waits).into());
        }
        if builds.is_empty() && given.dropped.is_empty() {
            return Ok(None);
        }
        let mut proposed = state.builds.get(&kernel.id).cloned().unwrap_or_default();
        for (_, extension) in &builds {
            proposed.insert(extension.descriptor.name.clone(), extension.id.clone());
        }
        for name in &given.dropped {
            proposed.remove(name);
        }
        ensure!(
            proposed.len() <= MAX_EXTENSIONS,
            "too many driver extensions for one kernel"
        );
        let mut pending = state.builds;
        if proposed.is_empty() {
            pending.remove(&kernel.id);
        } else {
            pending.insert(kernel.id.clone(), proposed);
        }
        Ok(Some(Pairing {
            builds,
            pending,
            staged: given.staged.clone(),
        }))
    }

    /// Publish a pairing's builds from `objects`, by digest, or else from
    /// the pairing's staged objects, then name it as the change paired with
    /// the deployment `deployment`.
    pub(super) fn publish_pairing(
        &self,
        pairing: Pairing,
        objects: &Path,
        deployment: &str,
    ) -> Result<()> {
        let object = |digest: &str| {
            let given = objects.join(digest);
            match &pairing.staged {
                Some(staged) if given.symlink_metadata().is_err() => staged.join(digest),
                _ => given,
            }
        };
        for (descriptor, extension) in &pairing.builds {
            let content = &extension.descriptor.content;
            self.publish_build(
                descriptor,
                extension,
                &object(&content.image.sha256),
                &object(&content.signature.sha256),
            )?;
        }
        let mut state = ext::read_state(&self.system)?;
        state.pending = Some(pairing.pending);
        state.pair = Some(deployment.to_owned());
        ext::write_state(&self.system, &state)
    }

    /// Stop installing builds for kernels no retained deployment runs, and a
    /// pending change paired with a deployment that is not retained, then
    /// remove every build's objects the state does not name and `mapped`
    /// does not hold (what this boot mapped), and what an interrupted
    /// install left. Answer how many builds went. The caller holds the
    /// transaction lock and writable mounts.
    ///
    /// # Errors
    ///
    /// The retained deployments cannot be read, or a removal that fails.
    pub fn collect_extensions(
        &self,
        mapped: &BTreeSet<String>,
        keys: &[[u8; 32]],
    ) -> Result<usize> {
        let held = ext::read_state(&self.system)?;
        let mut state = held.clone();
        let retained = self.retained(keys)?;
        let kernels: BTreeSet<&str> = retained.values().map(|kernel| kernel.id.as_str()).collect();
        state
            .builds
            .retain(|kernel, _| kernels.contains(kernel.as_str()));
        // A change alone keeps its kernels: its trial is the digest of them.
        if state
            .pair
            .as_ref()
            .is_some_and(|pair| !retained.contains_key(pair))
        {
            state.pending = None;
            state.pair = None;
        }
        if state != held {
            ext::write_state(&self.system, &state)?;
        }
        let keep: BTreeSet<&str> = state
            .ids()
            .into_iter()
            .chain(mapped.iter().map(String::as_str))
            .collect();
        self.remove_extension_objects(&keep)
    }

    /// Remove every extension and every enrolled owner certificate: the
    /// state first, so nothing names an object that goes. The caller holds
    /// the transaction lock and writable mounts.
    ///
    /// # Errors
    ///
    /// A removal that fails.
    pub fn remove_all_extensions(&self) -> Result<usize> {
        ext::write_state(&self.system, &ext::State::default())?;
        let removed = self.remove_extension_objects(&BTreeSet::new())?;
        remove_tree(&ext::dir(&self.system))?;
        remove_tree(&self.system.join(crate::boot::trust::OWNER_CERTS_DIR))?;
        Ok(removed)
    }

    fn remove_extension_objects(&self, keep: &BTreeSet<&str>) -> Result<usize> {
        let mut removed = BTreeSet::new();
        let objects = ext::objects_dir(&self.system);
        if objects.symlink_metadata().is_ok() {
            for entry in fs::read_dir(&objects)? {
                let entry = entry?;
                let name = entry.file_name();
                let id = name.to_str().unwrap_or_default().to_owned();
                ensure!(entry.file_type()?.is_dir(), "unexpected extension object");
                if keep.contains(id.as_str()) {
                    // An interrupted write of a build that stays.
                    for child in fs::read_dir(entry.path())? {
                        let child = child?.path();
                        if matches!(
                            child.extension().and_then(|e| e.to_str()),
                            Some("partial" | "pending")
                        ) {
                            fs::remove_file(child)?;
                            sync_dir(&entry.path())?;
                        }
                    }
                    continue;
                }
                remove_tree(&entry.path())?;
                removed.insert(id);
            }
        }
        if let Some(ubi) = self.ubi() {
            for volume in ubi.volumes()? {
                if let Some(id) = volume.name.strip_prefix(VOLUME_PREFIX)
                    && !keep.contains(id)
                {
                    ubi.remove(volume.id)?;
                    removed.insert(id.to_owned());
                }
            }
        }
        Ok(removed.len())
    }
}

/// Remove the directory `path` and everything in it, following no link.
fn remove_tree(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_err() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_tree(&entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    sync_dir(path)?;
    fs::remove_dir(path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}
