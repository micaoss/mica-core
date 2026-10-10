//! Installing a verified deployment's objects.

use crate::fit_env::{Environment, Record};
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

impl DeploymentStore {
    pub fn object_paths<'a>(
        &self,
        deployment: &'a crate::components::Deployment,
    ) -> Vec<(PathBuf, &'a crate::components::Artifact)> {
        let mut paths = vec![
            (
                self.system
                    .join(format!("roots/{}/rootfs.img", deployment.rootfs.id)),
                &deployment.rootfs.content.image,
            ),
            (
                self.system.join(format!(
                    "roots/{}/rootfs.roothash.p7s",
                    deployment.rootfs.id
                )),
                &deployment.rootfs.content.signature,
            ),
            (
                self.system
                    .join(format!("kernels/{}/support.img", deployment.kernel.id)),
                &deployment.kernel.support.image,
            ),
            (
                self.system.join(format!(
                    "kernels/{}/support.roothash.p7s",
                    deployment.kernel.id
                )),
                &deployment.kernel.support.signature,
            ),
            (
                match &self.boot {
                    BootBackend::Uefi { esp } => {
                        esp.join(format!("EFI/mica/kernels/{}.efi", deployment.kernel.id))
                    }
                    BootBackend::Fit { .. } => self
                        .system
                        .join(format!("kernels/{}/boot.itb", deployment.kernel.id)),
                },
                &deployment.kernel.boot.artifact,
            ),
        ];
        for core in &deployment.core {
            paths.push((
                self.system.join(format!("cores/{}/core.img", core.id)),
                &core.content.image,
            ));
            paths.push((
                self.system
                    .join(format!("cores/{}/core.roothash.p7s", core.id)),
                &core.content.signature,
            ));
        }
        paths
    }

    /// Caller owns the DATA transaction lock and writable SYSTEM/ESP mounts.
    /// Object sources are addressed only by authenticated SHA-256 digests.
    pub fn install(
        &self,
        envelope: &[u8],
        keys: &[[u8; 32]],
        target: &Target,
        objects: &Path,
        receipt: &BootReceipt,
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
                    BootBackend::Fit { .. } => "fit",
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
        // One thing on trial at a time: a boot that does not reach healthy has
        // one cause and one thing to undo.
        let cores = crate::core_state::read_state(&self.system)?;
        ensure!(
            cores.pending.is_none(),
            "a core set is on trial; let it settle before a system update"
        );
        match &cores.current {
            // The device's set must select on the new root, or the new
            // deployment would boot with nothing the runkit may compose.
            Some(current) => {
                let (set, _) =
                    crate::core_state::load_set(&self.system, current, keys, target.arch)?;
                crate::core_set::select(&set, target.features, deployment.rootfs.level())
                    .context(
                        "the current core set does not run on this deployment's root; update the core set first",
                    )?;
            }
            None => ensure!(
                deployment.schema == "mica/deployment/v1",
                "this deployment names no core components and the device holds no core set; install a core set first"
            ),
        }
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
        for (target, artifact) in &files {
            if target.symlink_metadata().is_ok() {
                verify_file(target, artifact)?;
                continue;
            }
            verify_file(&objects.join(&artifact.sha256), artifact)?;
            if target.starts_with(&self.system) {
                system_bytes += artifact.bytes;
            } else {
                esp_bytes += artifact.bytes;
            }
        }
        let collection = self.collection(
            receipt,
            keys,
            BTreeSet::from([receipt.deployment_id.clone()]),
            Some(&deployment),
        )?;
        let mut destinations = vec![(&self.system, system_bytes, self.reserves.system())];
        if let BootBackend::Uefi { esp } = &self.boot {
            destinations.push((esp, esp_bytes, self.reserves.esp()));
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
        for (target, artifact) in &files {
            publish_object(&objects.join(&artifact.sha256), target, artifact)?;
        }
        let mut hashes = vec![
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
        for core in &deployment.core {
            hashes.push((
                self.system.join(format!("cores/{}/core.roothash", core.id)),
                &core.content.root_hash,
            ));
        }
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
            BootBackend::Fit { firmware, layout } => {
                let mut env = Environment::load(firmware, *layout)?;
                env.records.insert(
                    0,
                    Record {
                        id: id.clone(),
                        kernel_id: deployment.kernel.id,
                        generation: deployment.generation,
                        tries_left: Some(3),
                    },
                );
                env.save(firmware)?;
            }
        }
        state.highest_generation = deployment.generation;
        state.candidate = Some(id);
        self.save_state(&state)?;
        Ok(state)
    }
}
