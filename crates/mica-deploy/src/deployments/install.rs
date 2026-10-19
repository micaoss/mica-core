//! Installing a verified deployment's objects.

use crate::fit_env::Record;
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use super::*;

pub fn verify_file(path: &Path, artifact: &crate::components::Artifact) -> Result<()> {
    let meta = path.symlink_metadata()?;
    ensure!(
        meta.is_file() && meta.len() == artifact.bytes,
        "object length or type mismatch: {}",
        path.display()
    );
    let mut file = File::open(path)?;
    let mut digest = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let mut buffer = [0_u8; 65536];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        ensure!(bytes <= artifact.bytes, "object grew during verification");
        digest.update(&buffer[..read]);
    }
    ensure!(
        bytes == artifact.bytes && hex::encode(digest.finish()) == artifact.sha256,
        "object digest mismatch: {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn directory(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty() && *parent != path)
    {
        directory(parent)?;
    }
    if path.symlink_metadata().is_ok() {
        ensure!(
            path.symlink_metadata()?.is_dir(),
            "invalid object directory"
        );
    } else {
        fs::create_dir(path)?;
        if let Some(parent) = path.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

pub(super) fn publish_object(
    source: &Path,
    target: &Path,
    artifact: &crate::components::Artifact,
) -> Result<()> {
    if target.symlink_metadata().is_ok() {
        return verify_file(target, artifact);
    }
    let parent = target.parent().context("object has no parent")?;
    directory(parent)?;
    let pending = target.with_extension("partial");
    if pending.symlink_metadata().is_ok() {
        ensure!(
            pending.symlink_metadata()?.is_file(),
            "invalid partial object"
        );
        fs::remove_file(&pending)?;
    }
    let mut input = File::open(source)?.take(artifact.bytes + 1);
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    verify_file(&pending, artifact)?;
    fs::rename(&pending, target)?;
    Ok(sync_dir(parent)?)
}

/// Which core set a new deployment boots with.
#[derive(Debug)]
pub enum CoreChoice {
    /// The device's set, unchanged.
    Keep,
    /// A newer general set, installed with the deployment as one pair.
    Pair {
        id: String,
        set: crate::update_set::UpdateSet,
        envelope: Vec<u8>,
    },
}

/// Where an installed object is held: a file on SYSTEM or the ESP, or on a
/// `ubi` board a static volume.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    File(PathBuf),
    Volume(String),
}

impl DeploymentStore {
    /// The UBI device of a `ubi` board.
    pub(crate) fn ubi(&self) -> Option<&dyn crate::ubi::UbiDevice> {
        match &self.boot {
            BootBackend::Ubi { ubi } => Some(ubi.as_ref()),
            _ => None,
        }
    }

    /// A large object's place: `file` under SYSTEM, or on a `ubi` board the
    /// static volume `volume`.
    pub(crate) fn large_object(&self, file: String, volume: String) -> Place {
        match self.ubi() {
            Some(_) => Place::Volume(volume),
            None => Place::File(self.system.join(file)),
        }
    }

    /// Whether something is held at `place`, sound or not.
    pub fn held(&self, place: &Place) -> Result<bool> {
        Ok(match place {
            Place::File(path) => path.symlink_metadata().is_ok(),
            Place::Volume(name) => crate::ubi::find(
                self.ubi().context("UBI object on a board without UBI")?,
                name,
            )?
            .is_some(),
        })
    }

    /// Hold what is at `place` to `artifact`.
    pub fn verify_held(&self, place: &Place, artifact: &crate::components::Artifact) -> Result<()> {
        match place {
            Place::File(path) => verify_file(path, artifact),
            Place::Volume(name) => crate::ubi::verify_object(
                self.ubi().context("UBI object on a board without UBI")?,
                name,
                artifact,
            ),
        }
    }

    /// Publish `source` at `place`, or verify what is already there.
    pub(crate) fn publish(
        &self,
        source: &Path,
        place: &Place,
        artifact: &crate::components::Artifact,
    ) -> Result<()> {
        match place {
            Place::File(path) => publish_object(source, path, artifact),
            Place::Volume(name) => crate::ubi::publish_object(
                self.ubi().context("UBI object on a board without UBI")?,
                source,
                name,
                artifact,
            ),
        }
    }

    /// A path the bytes at `place` read from, when something is there.
    pub fn readable(&self, place: &Place) -> Result<Option<PathBuf>> {
        Ok(match place {
            Place::File(path) => Some(path.clone()),
            Place::Volume(name) => {
                let ubi = self.ubi().context("UBI object on a board without UBI")?;
                crate::ubi::find(ubi, name)?.map(|volume| ubi.path(volume.id))
            }
        })
    }

    pub fn object_paths<'a>(
        &self,
        deployment: &'a crate::components::Deployment,
    ) -> Vec<(Place, &'a crate::components::Artifact)> {
        let paths = vec![
            (
                self.large_object(
                    format!("roots/{}/rootfs.img", deployment.rootfs.id),
                    format!("r-{}", deployment.rootfs.id),
                ),
                &deployment.rootfs.content.image,
            ),
            (
                Place::File(self.system.join(format!(
                    "roots/{}/rootfs.roothash.p7s",
                    deployment.rootfs.id
                ))),
                &deployment.rootfs.content.signature,
            ),
            (
                self.large_object(
                    format!("kernels/{}/support.img", deployment.kernel.id),
                    format!("s-{}", deployment.kernel.id),
                ),
                &deployment.kernel.support.image,
            ),
            (
                Place::File(self.system.join(format!(
                    "kernels/{}/support.roothash.p7s",
                    deployment.kernel.id
                ))),
                &deployment.kernel.support.signature,
            ),
            (
                match &self.boot {
                    BootBackend::Uefi { esp } => Place::File(
                        esp.join(format!("EFI/mica/kernels/{}.efi", deployment.kernel.id)),
                    ),
                    BootBackend::Fit { .. } | BootBackend::Ubi { .. } => self.large_object(
                        format!("kernels/{}/boot.itb", deployment.kernel.id),
                        format!("k-{}", deployment.kernel.id),
                    ),
                },
                &deployment.kernel.boot.artifact,
            ),
        ];
        paths
    }

    /// The core set `deployment`'s image carries, when a system update to it
    /// takes a newer set: the device follows `general`, and the image's set is
    /// a `general` one of a higher generation than the device's.
    pub fn newer_image_set<'a>(
        &self,
        deployment: &'a crate::components::Deployment,
        keys: &[[u8; 32]],
        arch: &str,
    ) -> Result<Option<&'a crate::components::CoreSetRef>> {
        use crate::update_set::{GENERAL, SetKind};
        let Some(image) = deployment
            .core_set
            .as_ref()
            .filter(|image| image.channel == GENERAL)
        else {
            return Ok(None);
        };
        let kind = SetKind::Core;
        let Some(current) = crate::set_state::read_state(&self.system, kind)?.current else {
            return Ok(None);
        };
        let (held, _) = crate::set_state::load_set(&self.system, kind, &current, keys, arch)?;
        Ok((held.channel == GENERAL && held.generation < image.generation).then_some(image))
    }

    /// The set `image` names, from its envelope in `objects`: authenticated,
    /// and the set it names.
    pub fn image_set(
        &self,
        image: &crate::components::CoreSetRef,
        objects: &Path,
        keys: &[[u8; 32]],
        arch: &str,
    ) -> Result<(crate::update_set::UpdateSet, Vec<u8>)> {
        let artifact = crate::components::Artifact {
            sha256: image.sha256.clone(),
            bytes: image.bytes,
        };
        let path = objects.join(&artifact.sha256);
        verify_file(&path, &artifact).context("the deployment's core set is not at hand")?;
        let envelope = read_bounded(&path, crate::set_state::MAX_ENVELOPE_BYTES)?;
        let set = crate::update_set::authenticate(&envelope, keys)?;
        ensure!(
            set.id()? == image.id
                && set.kind == crate::update_set::SetKind::Core
                && set.channel == image.channel
                && set.generation == image.generation
                && set.arch == arch,
            "the deployment's core set differs from what it names"
        );
        Ok((set, envelope))
    }

    /// Which core set `deployment` boots with (`docs/mica-core.md` 6.2): the
    /// device's set, unless the device follows `general` at a generation
    /// below the image's set. Then the newest general set that selects on the
    /// new root: `latest`, the channel's latest set fetched for it, else the
    /// image's own from `objects`. A set that must run on the new root and
    /// does not refuses the installation, naming the set and the constraint.
    pub fn core_choice(
        &self,
        deployment: &crate::components::Deployment,
        keys: &[[u8; 32]],
        target: &Target,
        objects: &Path,
        latest: Option<&[u8]>,
    ) -> Result<CoreChoice> {
        use crate::update_set::{GENERAL, SetKind, authenticate};
        let kind = SetKind::Core;
        let root = deployment.rootfs.facts();
        let cores = crate::set_state::read_state(&self.system, kind)?;
        // One thing on trial at a time: a boot that does not reach healthy has
        // one cause and one thing to undo.
        ensure!(
            cores.pending.is_none(),
            "a set is on trial; let it settle before a system update"
        );
        let current = cores
            .current
            .as_ref()
            .context("this device holds no core set; install one first")?;
        let Some(image) = self.newer_image_set(deployment, keys, target.arch)? else {
            // A system update never downgrades core, nor moves a device off
            // its channel: the device's set must run on the new root.
            let (held, _) =
                crate::set_state::load_set(&self.system, kind, current, keys, target.arch)?;
            crate::core_set::select(&held, target.features, &root).with_context(|| {
                format!("the device's core set {current} does not run on this deployment's root")
            })?;
            return Ok(CoreChoice::Keep);
        };
        let (image_set, image_envelope) = self.image_set(image, objects, keys, target.arch)?;
        let mut refusals = Vec::new();
        // A latest set that is not a general one at or above the image's is
        // not the newer of the two, and is passed over.
        let latest = latest
            .map(|envelope| authenticate(envelope, keys).map(|set| (envelope, set)))
            .transpose()?
            .filter(|(_, set)| {
                set.kind == kind
                    && set.arch == target.arch
                    && set.channel == GENERAL
                    && set.generation >= image.generation
            });
        if let Some((latest, set)) = latest {
            let id = set.id()?;
            match crate::core_set::select(&set, target.features, &root) {
                Ok(_) => {
                    return Ok(CoreChoice::Pair {
                        id,
                        set,
                        envelope: latest.to_vec(),
                    });
                }
                Err(error) => refusals.push(format!("core set {id}: {error}")),
            }
        }
        match crate::core_set::select(&image_set, target.features, &root) {
            Ok(_) => Ok(CoreChoice::Pair {
                id: image.id.clone(),
                set: image_set,
                envelope: image_envelope,
            }),
            Err(error) => {
                refusals.push(format!("core set {}: {error}", image.id));
                anyhow::bail!(
                    "no general core set runs on this deployment's root: {}",
                    refusals.join("; ")
                )
            }
        }
    }

    /// Caller owns the DATA transaction lock and writable SYSTEM/ESP mounts.
    /// Object sources are addressed only by authenticated SHA-256 digests.
    /// `latest` is the core channel's latest set, fetched for this
    /// deployment ([`Self::core_choice`]).
    pub fn install(
        &self,
        envelope: &[u8],
        keys: &[[u8; 32]],
        target: &Target,
        objects: &Path,
        receipt: &BootReceipt,
        latest: Option<&[u8]>,
    ) -> Result<State> {
        use crate::components::{authenticate_deployment, component_id};
        let deployment = authenticate_deployment(envelope, keys)?;
        ensure!(
            deployment.board == target.board && deployment.arch == target.arch,
            "deployment targets another device"
        );
        ensure!(
            deployment.product == target.product,
            "deployment targets another product"
        );
        ensure!(
            deployment.kernel.boot.format
                == match self.boot {
                    BootBackend::Uefi { .. } => "uki",
                    BootBackend::Fit { .. } | BootBackend::Ubi { .. } => "fit",
                },
            "deployment boot format differs from this device"
        );
        let id = component_id(&serde_json::to_value(&deployment)?)?;
        let mut state = self.effective_state()?;
        let entries = self.entries()?;
        ensure!(
            !entries.iter().any(|e| e.id == id) && !state.failed.contains(&id),
            "deployment is installed or rejected"
        );
        ensure!(state.candidate.is_none(), "another deployment is pending");
        let choice = self.core_choice(&deployment, keys, target, objects, latest)?;
        self.validate_receipt(receipt)?;
        ensure!(
            state.current.as_deref() == Some(receipt.deployment_id.as_str())
                && entries
                    .iter()
                    .any(|e| e.id == receipt.deployment_id && e.tries_left.is_none())
                && !state.failed.contains(&receipt.deployment_id),
            "confirm the running deployment before installation"
        );
        ensure!(
            deployment.generation > state.highest_generation
                && entries.iter().all(|e| deployment.generation > e.generation),
            "deployment generation is not newer"
        );
        ensure!(entries.len() <= 2, "retained deployment set is full");
        let files = self.object_paths(&deployment);
        // Validate every missing input and reserve both destination filesystems
        // before exposing any candidate. Reused objects require no source copy.
        let mut system_bytes = 0_u64;
        let mut esp_bytes = 0_u64;
        let mut lebs = 0_u64;
        for (target, artifact) in &files {
            if self.held(target)? {
                self.verify_held(target, artifact)?;
                continue;
            }
            verify_file(&objects.join(&artifact.sha256), artifact)?;
            match (target, self.ubi()) {
                (Place::Volume(_), Some(ubi)) => lebs += crate::ubi::lebs(ubi, artifact.bytes),
                (Place::File(path), _) if path.starts_with(&self.system) => {
                    system_bytes += artifact.bytes;
                }
                _ => esp_bytes += artifact.bytes,
            }
        }
        let collection = self.collection(
            receipt,
            keys,
            BTreeSet::from([receipt.deployment_id.clone()]),
            Some(&deployment),
        )?;
        // On UBI the reserve is kept in the device's free erase blocks, where
        // the objects go; SYSTEM's volume holds only small files.
        let mut destinations = vec![(
            &self.system,
            system_bytes,
            if self.ubi().is_some() {
                0
            } else {
                self.reserves.system()
            },
        )];
        if let BootBackend::Uefi { esp } = &self.boot {
            destinations.push((esp, esp_bytes, self.reserves.esp()));
        }
        if let Some(ubi) = self.ubi() {
            ensure!(
                ubi.available_lebs()? + collection.reclaimed_lebs()
                    >= lebs + crate::ubi::lebs(ubi, self.reserves.system()),
                "insufficient UBI space"
            );
        }
        for &(path, bytes, reserve) in &destinations {
            directory(path)?;
            let space = rustix::fs::statvfs(path)?;
            ensure!(
                space.f_bavail * space.f_frsize + collection.reclaimed_bytes(path)?
                    >= bytes + reserve,
                "insufficient destination space: {}",
                path.display()
            );
            ensure!(
                space.f_files == 0 || space.f_favail >= 32,
                "insufficient destination inodes"
            );
        }
        // No new object is written until old B is durably unbootable. Native
        // records are authoritative if DATA reconciliation is interrupted.
        self.retain_entries(&entries, &[Some(&receipt.deployment_id)])?;
        state.fallback = None;
        state.candidate = None;
        self.save_state(&state)?;
        collection.apply()?;
        for &(path, bytes, reserve) in &destinations {
            let space = rustix::fs::statvfs(path)?;
            ensure!(
                space.f_bavail * space.f_frsize >= bytes + reserve,
                "insufficient reclaimed destination space: {}",
                path.display()
            );
        }
        if let Some(ubi) = self.ubi() {
            ensure!(
                ubi.available_lebs()? >= lebs + crate::ubi::lebs(ubi, self.reserves.system()),
                "insufficient reclaimed UBI space"
            );
        }
        for (target, artifact) in &files {
            self.publish(&objects.join(&artifact.sha256), target, artifact)?;
        }
        let hashes = [
            (
                self.system
                    .join(format!("roots/{}/rootfs.roothash", deployment.rootfs.id)),
                &deployment.rootfs.content.root_hash,
            ),
            (
                self.system
                    .join(format!("kernels/{}/support.roothash", deployment.kernel.id)),
                &deployment.kernel.support.root_hash,
            ),
        ];
        for (path, hash) in hashes {
            if path.try_exists()? {
                ensure!(
                    read_bounded(&path, 64)? == hash.as_bytes(),
                    "existing root hash differs"
                );
            } else {
                atomic_write(&path, hash.as_bytes())?;
            }
        }
        directory(&self.system.join("deployments"))?;
        let descriptor = self.system.join(format!("deployments/{id}.json"));
        if descriptor.try_exists()? {
            ensure!(
                read_bounded(&descriptor, 24576)? == envelope,
                "deployment ID was reused"
            );
        } else {
            atomic_write(&descriptor, envelope)?;
        }
        sync_dir(&self.system)?;
        // The new deployment's set is named before its entry exists: a pair
        // whose deployment never appears is dropped by the next confirmation.
        let kind = crate::update_set::SetKind::Core;
        let mut cores = crate::set_state::read_state(&self.system, kind)?;
        cores.pair = None;
        cores
            .bound
            .retain(|deployment, _| deployment == &receipt.deployment_id);
        if let CoreChoice::Pair {
            id: set_id,
            set,
            envelope,
        } = &choice
        {
            self.publish_set(set, envelope, objects)?;
            cores.pair = Some(id.clone());
            cores.pending = Some(set_id.clone());
        }
        crate::set_state::write_state(&self.system, kind, &cores)?;
        // The entry is the activation commit. All referenced bytes are durable.
        match &self.boot {
            BootBackend::Uefi { esp } => {
                sync_dir(&esp.join("EFI/mica/kernels"))?;
                let entry = format!(
                    "title MICA {}\nversion {}\nsort-key mica\nefi /EFI/mica/kernels/{}.efi\n",
                    deployment.version, deployment.generation, deployment.kernel.id
                );
                atomic_write(
                    &esp.join(format!("loader/entries/mica-{id}+3.conf")),
                    entry.as_bytes(),
                )?;
            }
            BootBackend::Fit { .. } | BootBackend::Ubi { .. } => {
                let mut env = self.boot.records()?;
                env.records.insert(
                    0,
                    Record {
                        id: id.clone(),
                        kernel_id: deployment.kernel.id,
                        generation: deployment.generation,
                        tries_left: Some(3),
                    },
                );
                self.boot.save_records(&env)?;
            }
        }
        state.highest_generation = deployment.generation;
        state.candidate = Some(id);
        self.save_state(&state)?;
        Ok(state)
    }
}
