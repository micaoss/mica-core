//! Describing, confirming, collecting, rolling back and rejecting deployments.

use crate::boot::selected_entry;
use crate::fit_env::Environment;
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, fs};

use super::*;

impl DeploymentStore {
    pub fn describe(&self, keys: &[[u8; 32]]) -> Result<Vec<DeploymentStatus>> {
        use crate::components::{authenticate_deployment, component_id};
        self.entries()?
            .into_iter()
            .map(|entry| {
                let deployment = authenticate_deployment(
                    &read_bounded(
                        &self.system.join(format!("deployments/{}.json", entry.id)),
                        24576,
                    )?,
                    keys,
                )?;
                ensure!(
                    component_id(&serde_json::to_value(&deployment)?)? == entry.id
                        && deployment.generation == entry.generation,
                    "deployment status identity mismatch"
                );
                if let BootBackend::Fit { firmware, layout } = &self.boot {
                    ensure!(
                        Environment::load(firmware, *layout)?
                            .records
                            .iter()
                            .any(|r| r.id == entry.id && r.kernel_id == deployment.kernel.id),
                        "deployment boot record kernel mismatch"
                    );
                }
                Ok(DeploymentStatus {
                    entry,
                    version: deployment.version,
                    kernel_id: deployment.kernel.id,
                    kernel_release: deployment.kernel.release,
                    rootfs_id: deployment.rootfs.id,
                })
            })
            .collect()
    }

    pub fn confirm(&self, receipt: &BootReceipt) -> Result<State> {
        self.validate_receipt(receipt)?;
        let mut state = self.effective_state()?;
        let entries = self.entries()?;
        for entry in &entries {
            if entry.id != receipt.deployment_id
                && entry.tries_left == Some(0)
                && !state.failed.contains(&entry.id)
            {
                if state.failed.len() == 128 {
                    state.failed.remove(0);
                }
                state.failed.push(entry.id.clone());
            }
        }
        if state
            .candidate
            .as_ref()
            .is_some_and(|id| state.failed.contains(id))
        {
            state.candidate = None;
        }
        let current = entries
            .iter()
            .find(|e| e.id == receipt.deployment_id)
            .context("running deployment entry is missing")?;
        ensure!(current.tries_left != Some(3), "entry was never launched");
        ensure!(
            !state.failed.contains(&current.id),
            "deployment is already rejected"
        );
        let fallback = if state.current.as_deref() == Some(&current.id) {
            state.fallback.clone().filter(|id| {
                entries
                    .iter()
                    .any(|e| &e.id == id && e.id != current.id && e.tries_left != Some(0))
            })
        } else {
            state
                .current
                .clone()
                .filter(|id| {
                    entries
                        .iter()
                        .any(|e| &e.id == id && e.tries_left != Some(0))
                })
                .or_else(|| {
                    entries
                        .iter()
                        .find(|e| e.id != current.id && e.tries_left != Some(0))
                        .map(|e| e.id.clone())
                })
        };
        // The uncounted entry is the durable confirmation fact. If DATA recording
        // is interrupted, the next confirmation reconciles it without refilling.
        if current.tries_left.is_some() {
            self.set_tries(current, None)?;
        }
        state.current = Some(current.id.clone());
        state.fallback = fallback;
        if state.candidate.as_deref() == Some(&current.id) {
            state.candidate = None;
        }
        self.save_state(&state)?;
        self.retain_entries(
            &entries,
            &[
                state.current.as_deref(),
                state.fallback.as_deref(),
                state.candidate.as_deref(),
            ],
        )?;
        Ok(state)
    }

    /// Collect unreachable immutable objects after validating every retained
    /// descriptor. The caller holds the transaction lock and writable mounts.
    pub fn collect(&self, receipt: &BootReceipt, keys: &[[u8; 32]]) -> Result<usize> {
        let state = self.effective_state()?;
        let mut retained = BTreeSet::from([receipt.deployment_id.clone()]);
        for id in [state.current, state.fallback, state.candidate]
            .into_iter()
            .flatten()
        {
            retained.insert(id);
        }
        retained.extend(self.entries()?.into_iter().map(|entry| entry.id));
        let collection = self.collection(receipt, keys, retained, None)?;
        let entries = self.entries()?;
        self.retain_entries(
            &entries,
            &entries
                .iter()
                .map(|e| Some(e.id.as_str()))
                .collect::<Vec<_>>(),
        )?;
        collection.apply()
    }

    pub(super) fn collection(
        &self,
        receipt: &BootReceipt,
        keys: &[[u8; 32]],
        retained: BTreeSet<String>,
        incoming: Option<&crate::components::Deployment>,
    ) -> Result<Collection> {
        use crate::components::{authenticate_deployment, component_id};
        self.validate_receipt(receipt)?;
        let mut roots = BTreeSet::new();
        let mut kernels = BTreeSet::new();
        let mut cores = BTreeSet::new();
        let mut target = None;
        for id in &retained {
            valid_id(id)?;
            let path = self.system.join(format!("deployments/{id}.json"));
            let d = authenticate_deployment(&read_bounded(&path, 24576)?, keys)?;
            ensure!(
                component_id(&serde_json::to_value(&d)?)? == *id,
                "retained descriptor identity mismatch"
            );
            let board = (d.board, d.arch);
            if let Some(expected) = &target {
                ensure!(expected == &board, "retained deployment target mismatch");
            } else {
                target = Some(board);
            }
            if *id == receipt.deployment_id {
                ensure!(
                    d.kernel.id == receipt.kernel_id && d.rootfs.id == receipt.rootfs_id,
                    "running component identity mismatch"
                );
            }
            cores.extend(d.core.iter().map(|core| core.id.clone()));
            roots.insert(d.rootfs.id);
            kernels.insert(d.kernel.id);
        }
        if let Some(deployment) = incoming {
            ensure!(
                target.as_ref() == Some(&(deployment.board.clone(), deployment.arch.clone())),
                "incoming deployment target mismatch"
            );
            // A new deployment can reuse components belonging only to old B.
            // Keep those verified objects while retiring B's descriptor.
            roots.insert(deployment.rootfs.id.clone());
            kernels.insert(deployment.kernel.id.clone());
            cores.extend(deployment.core.iter().map(|core| core.id.clone()));
        }
        // The device's core sets hold their components as deployments do.
        cores.extend(self.core_set_components(keys)?);
        let mut files = Vec::new();
        let mut directories = Vec::new();
        for (parent, keep) in [
            (self.system.join("roots"), &roots),
            (self.system.join("kernels"), &kernels),
            (self.system.join("cores"), &cores),
        ] {
            // A SYSTEM that never held a core component has no namespace for them.
            if parent.ends_with("cores") && parent.symlink_metadata().is_err() {
                continue;
            }
            ensure!(
                parent.symlink_metadata()?.is_dir(),
                "invalid object namespace"
            );
            for entry in fs::read_dir(&parent)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid object name"))?;
                valid_id(&name)?;
                ensure!(entry.file_type()?.is_dir(), "object directory is not real");
                let referenced = keep.contains(&name);
                for child in fs::read_dir(entry.path())? {
                    let child = child?;
                    ensure!(child.file_type()?.is_file(), "unexpected object child");
                    let path = child.path();
                    if !referenced
                        || matches!(
                            path.extension().and_then(|s| s.to_str()),
                            Some("partial" | "pending")
                        )
                    {
                        files.push(path);
                    }
                    ensure!(files.len() <= 4096, "collection exceeds file limit");
                }
                if !referenced {
                    directories.push(entry.path());
                }
                ensure!(directories.len() <= 512, "collection exceeds object limit");
            }
        }
        let mut artifacts = vec![(
            self.system.join("deployments"),
            ".json",
            ".pending",
            &retained,
        )];
        if let BootBackend::Uefi { esp } = &self.boot {
            artifacts.push((esp.join("EFI/mica/kernels"), ".efi", ".partial", &kernels));
        }
        for (parent, suffix, temporary_suffix, keep) in artifacts {
            ensure!(
                parent.symlink_metadata()?.is_dir(),
                "invalid artifact namespace"
            );
            for entry in fs::read_dir(&parent)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid artifact name"))?;
                let temporary = name.ends_with(temporary_suffix);
                let id = name
                    .strip_suffix(if temporary { temporary_suffix } else { suffix })
                    .context("unexpected artifact name")?;
                valid_id(id)?;
                ensure!(entry.file_type()?.is_file(), "artifact is not regular");
                if temporary || !keep.contains(id) {
                    files.push(entry.path());
                }
                ensure!(files.len() <= 4096, "collection exceeds file limit");
            }
        }
        if let BootBackend::Uefi { esp } = &self.boot {
            for entry in fs::read_dir(esp.join("loader/entries"))? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_str().context("invalid entry name")?;
                if let Some(stem) = name.strip_suffix(".pending") {
                    selected_entry(&format!("{stem}.conf"))?;
                    ensure!(entry.file_type()?.is_file(), "pending entry is not regular");
                    files.push(entry.path());
                    ensure!(files.len() <= 4096, "collection exceeds file limit");
                }
            }
        }
        let pending = self.meta.join("deployments.pending");
        if pending.symlink_metadata().is_ok() {
            ensure!(
                pending.symlink_metadata()?.is_file(),
                "pending state is not regular"
            );
            if incoming.is_none() {
                files.push(pending);
            }
        }
        Ok(Collection { files, directories })
    }

    pub fn rollback(&self, receipt: &BootReceipt) -> Result<State> {
        self.validate_receipt(receipt)?;
        let state = self.effective_state()?;
        ensure!(state.candidate.is_none(), "candidate deployment is pending");
        ensure!(
            state.current.as_ref() == Some(&receipt.deployment_id)
                && !state.failed.contains(&receipt.deployment_id),
            "running deployment is not confirmed"
        );
        let fallback = state.fallback.as_ref().context("no retained fallback")?;
        ensure!(
            fallback != &receipt.deployment_id
                && !state.failed.contains(fallback)
                && self
                    .entries()?
                    .iter()
                    .any(|entry| &entry.id == fallback && entry.tries_left != Some(0)),
            "no usable retained fallback"
        );
        self.reject(&receipt.deployment_id)
    }

    pub fn reject(&self, id: &str) -> Result<State> {
        valid_id(id)?;
        let mut state = self.effective_state()?;
        let entries = self.entries()?;
        let entry = entries
            .iter()
            .find(|e| e.id == id)
            .context("deployment entry is missing")?;
        ensure!(
            entries
                .iter()
                .any(|e| e.id != id && e.tries_left != Some(0)),
            "cannot reject the last usable deployment"
        );
        ensure!(
            state.fallback.as_deref() != Some(id),
            "cannot reject retained fallback"
        );
        if entry.tries_left != Some(0) {
            self.set_tries(entry, Some(0))?;
        }
        if !state.failed.iter().any(|failed| failed == id) {
            if state.failed.len() == 128 {
                state.failed.remove(0);
            }
            state.failed.push(id.to_owned());
        }
        if state.candidate.as_deref() == Some(id) {
            state.candidate = None;
        }
        self.save_state(&state)?;
        Ok(state)
    }
}
