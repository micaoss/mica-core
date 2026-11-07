//! What an acquisition on a `ubi` board needs of the store (`docs/mica-core.md`
//! 6.6). There a fetch writes each object the store keeps in a static volume
//! straight into it, so the objects of a descriptor staged in the workspace are
//! already the store's: collection keeps them, a fetch may make room for them
//! by retiring the fallback, and discarding the workspace releases them.

use anyhow::{Result, ensure};
use std::collections::BTreeSet;

use super::*;

/// The objects staged descriptors name: deployments' roots and kernels, and
/// the components of their image sets and of staged sets.
#[derive(Debug, Default)]
pub(crate) struct Staged {
    pub(crate) roots: BTreeSet<String>,
    pub(crate) kernels: BTreeSet<String>,
    pub(crate) components: BTreeSet<String>,
}

impl DeploymentStore {
    /// Keep the objects named by the descriptors staged in `staged`, the
    /// workspace's `verified/`, through collection.
    #[must_use]
    pub fn with_staged(mut self, staged: PathBuf) -> Self {
        self.staged = Some(staged);
        self
    }

    /// What the staged descriptors that authenticate name. One that cannot be
    /// read or does not authenticate names nothing: it can never be installed.
    pub(crate) fn staged(&self, keys: &[[u8; 32]]) -> Staged {
        let mut staged = Staged::default();
        let Some(directory) = &self.staged else {
            return staged;
        };
        let Ok(entries) = fs::read_dir(directory) else {
            return staged;
        };
        let set = |bytes: &[u8]| crate::update_set::authenticate(bytes, keys).ok();
        for entry in entries.flatten().take(4096) {
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            let Ok(bytes) = read_bounded(&entry.path(), crate::set_state::MAX_ENVELOPE_BYTES)
            else {
                continue;
            };
            if name.starts_with("set-") {
                if let Some(set) = set(&bytes) {
                    staged
                        .components
                        .extend(set.components.into_iter().map(|core| core.id));
                }
                continue;
            }
            let Ok(deployment) = crate::components::authenticate_deployment(&bytes, keys) else {
                continue;
            };
            if let Some(image) = &deployment.core_set
                && let Ok(bytes) = read_bounded(
                    &directory.join("objects").join(&image.sha256),
                    crate::set_state::MAX_ENVELOPE_BYTES,
                )
                && let Some(set) = set(&bytes)
            {
                staged
                    .components
                    .extend(set.components.into_iter().map(|core| core.id));
            }
            staged.roots.insert(deployment.rootfs.id);
            staged.kernels.insert(deployment.kernel.id);
        }
        staged
    }

    /// Retire the retained deployment that is not running and collect what
    /// only it held, as installing `incoming` would: a fetch on a `ubi` board
    /// does this when the objects it writes would not fit otherwise. The
    /// confirmed running deployment is never touched. The caller holds the
    /// transaction lock and writable SYSTEM.
    pub fn release_fallback(
        &self,
        receipt: &BootReceipt,
        keys: &[[u8; 32]],
        incoming: &crate::components::Deployment,
    ) -> Result<()> {
        let mut state = self.effective_state()?;
        let entries = self.entries()?;
        ensure!(state.candidate.is_none(), "another deployment is pending");
        ensure!(
            state.current.as_deref() == Some(receipt.deployment_id.as_str())
                && entries
                    .iter()
                    .any(|e| e.id == receipt.deployment_id && e.tries_left.is_none())
                && !state.failed.contains(&receipt.deployment_id),
            "confirm the running deployment before releasing its fallback"
        );
        let collection = self.collection(
            receipt,
            keys,
            BTreeSet::from([receipt.deployment_id.clone()]),
            Some(incoming),
        )?;
        self.retain_entries(&entries, &[Some(&receipt.deployment_id)])?;
        state.fallback = None;
        self.save_state(&state)?;
        collection.apply()?;
        Ok(())
    }

    /// On a `ubi` board, remove every object volume nothing holds: each `p.*`,
    /// and each `r-`, `k-`, `s-` or `c-` volume no retained deployment, held
    /// set or staged descriptor names. Discarding the workspace does this, so
    /// the objects a fetch wrote into the store go with its descriptors. It
    /// writes no file. The caller holds the transaction lock.
    pub fn release_volumes(&self, keys: &[[u8; 32]]) -> Result<usize> {
        let Some(ubi) = self.ubi() else {
            return Ok(0);
        };
        let state = self.effective_state()?;
        let mut retained: BTreeSet<String> =
            self.entries()?.into_iter().map(|entry| entry.id).collect();
        retained.extend(
            [state.current, state.fallback, state.candidate]
                .into_iter()
                .flatten(),
        );
        let Staged {
            mut roots,
            mut kernels,
            components: _,
        } = self.staged(keys);
        for id in &retained {
            valid_id(id)?;
            let deployment = crate::components::authenticate_deployment(
                &read_bounded(&self.system.join(format!("deployments/{id}.json")), 24576)?,
                keys,
            )?;
            roots.insert(deployment.rootfs.id);
            kernels.insert(deployment.kernel.id);
        }
        let components = self.held_components(crate::update_set::SetKind::Core, keys)?;
        let mut removed = 0;
        for volume in ubi.volumes()? {
            let unreferenced = |prefix: &str, keep: &BTreeSet<String>| {
                volume
                    .name
                    .strip_prefix(prefix)
                    .is_some_and(|id| valid_id(id).is_ok() && !keep.contains(id))
            };
            if volume.name.starts_with(crate::ubi::PENDING)
                || unreferenced("r-", &roots)
                || unreferenced("k-", &kernels)
                || unreferenced("s-", &kernels)
                || unreferenced("c-", &components)
            {
                ubi.remove(volume.id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}
