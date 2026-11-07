//! Installing, removing and collecting driver extensions on SYSTEM
//! (`docs/mica-core.md` 6.3; the layout is `crate::extensions`'s).

use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, fs, path::Path};

use super::*;
use crate::extensions::{self as ext, MAX_EXTENSIONS, VOLUME_PREFIX};

impl DeploymentStore {
    /// The kernel components of the deployments the boot records hold, by id.
    fn retained_kernels(
        &self,
        keys: &[[u8; 32]],
    ) -> Result<std::collections::BTreeMap<String, crate::components::KernelComponent>> {
        let mut kernels = std::collections::BTreeMap::new();
        for entry in self.entries()? {
            let path = self.system.join(format!("deployments/{}.json", entry.id));
            let deployment =
                crate::components::authenticate_deployment(&read_bounded(&path, 24576)?, keys)?;
            kernels.insert(deployment.kernel.id.clone(), deployment.kernel);
        }
        Ok(kernels)
    }

    /// Install the driver extension whose descriptor is `descriptor` for the
    /// kernel it is built for, with its image at `image` and its root hash
    /// signature at `signature`, replacing the build of the same name for
    /// that kernel; answer its extension id. The kernel must be one a
    /// retained deployment runs, and say that its runkit maps extensions.
    /// The signature is the kernel's to check, when the image is mapped at
    /// the next boot of that kernel. The caller holds the transaction lock
    /// and writable mounts.
    ///
    /// # Errors
    ///
    /// An invalid descriptor, a kernel no retained deployment runs or that
    /// maps no extensions, objects that are not the descriptor's, too many
    /// extensions, too little space, or a write that fails.
    pub fn install_extension(
        &self,
        descriptor: &[u8],
        image: &Path,
        signature: &Path,
        keys: &[[u8; 32]],
    ) -> Result<String> {
        let extension = crate::driver_ext::parse_descriptor(descriptor)?;
        let d = &extension.descriptor;
        let kernels = self.retained_kernels(keys)?;
        let kernel = kernels
            .get(&d.kernel)
            .context("no retained deployment runs the extension's kernel")?;
        ensure!(
            kernel.arch == d.arch,
            "the extension is built for another architecture"
        );
        ensure!(
            kernel
                .features
                .as_ref()
                .is_some_and(|features| features.iter().any(|f| f == ext::KERNEL_FEATURE)),
            "the extension's kernel does not map driver extensions"
        );
        let mut state = ext::read_state(&self.system)?;
        let builds = state.builds.entry(d.kernel.clone()).or_default();
        ensure!(
            builds.contains_key(&d.name) || builds.len() < MAX_EXTENSIONS,
            "too many driver extensions for one kernel"
        );
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
        builds.insert(d.name.clone(), extension.id.clone());
        ext::write_state(&self.system, &state)?;
        Ok(extension.id)
    }

    /// Stop installing the extension `name`: for the kernel `kernel`, or for
    /// every kernel. Answer how many builds the state no longer names. The
    /// caller holds the transaction lock and writable mounts.
    ///
    /// # Errors
    ///
    /// The state cannot be read or written.
    pub fn remove_extension(&self, name: &str, kernel: Option<&str>) -> Result<usize> {
        let mut state = ext::read_state(&self.system)?;
        let mut removed = 0;
        for (id, builds) in &mut state.builds {
            if kernel.is_none_or(|kernel| kernel == id) && builds.remove(name).is_some() {
                removed += 1;
            }
        }
        state.builds.retain(|_, builds| !builds.is_empty());
        if removed > 0 {
            ext::write_state(&self.system, &state)?;
        }
        Ok(removed)
    }

    /// Stop installing builds for kernels no retained deployment runs, then
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
        let mut state = ext::read_state(&self.system)?;
        let kernels = self.retained_kernels(keys)?;
        let named = state.builds.len();
        state
            .builds
            .retain(|kernel, _| kernels.contains_key(kernel));
        if state.builds.len() != named {
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
