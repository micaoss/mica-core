//! A factory reset of version 2 on the store (`docs/mica-core.md` 3.7): its
//! staging, which leaves nothing pending to boot, and its finish on the
//! first boot after DATA was made again, which leaves the running deployment
//! and its core set alone on SYSTEM.

use anyhow::{Result, ensure};
use std::collections::BTreeSet;

use super::*;
use crate::set_state::{self, SetState};
use crate::update_set::SetKind;

impl DeploymentStore {
    /// Make the store ready for a factory reset to the running deployment:
    /// it must be confirmed, and a candidate is rejected, so the boot into
    /// the reset boots it. The caller holds the transaction lock and
    /// writable mounts.
    pub fn prepare_factory_reset(&self, receipt: &BootReceipt) -> Result<()> {
        self.validate_receipt(receipt)?;
        let state = self.effective_state()?;
        ensure!(
            state.current.as_deref() == Some(receipt.deployment_id.as_str())
                && !state.failed.contains(&receipt.deployment_id)
                && self
                    .entries()?
                    .iter()
                    .any(|e| e.id == receipt.deployment_id && e.tries_left.is_none()),
            "confirm the running deployment before a factory reset"
        );
        if let Some(candidate) = state.candidate {
            self.reject(&candidate)?;
        }
        Ok(())
    }

    /// Finish a factory reset on SYSTEM: retire every deployment but the
    /// running one and collect what only they held, leave each kind of set
    /// with the running deployment's set alone, and remove every driver
    /// extension and every enrolled owner certificate, which the boot into
    /// the reset neither mapped nor loaded. Answer what was removed.
    /// Every step can run again. The caller holds the transaction lock and
    /// writable mounts.
    pub fn finish_factory_reset(&self, receipt: &BootReceipt, keys: &[[u8; 32]]) -> Result<usize> {
        self.validate_receipt(receipt)?;
        let running = receipt.deployment_id.clone();
        let mut state = self.effective_state()?;
        self.retain_entries(&self.entries()?, &[Some(&running)])?;
        state.current = Some(running.clone());
        state.fallback = None;
        state.candidate = None;
        self.save_state(&state)?;
        // Listed only once the state is written: a write a power cut left
        // behind is the state's to replace, not the collection's to remove.
        let collection = self.collection(receipt, keys, BTreeSet::from([running.clone()]), None)?;
        let mut removed = collection.apply()?;
        let kind = SetKind::Core;
        let held = set_state::read_state(&self.system, kind)?;
        let alone = SetState {
            current: held.bound.get(&running).or(held.current.as_ref()).cloned(),
            ..SetState::default()
        };
        if alone != held {
            set_state::write_state(&self.system, kind, &alone)?;
            set_state::end_trial(&self.meta)?;
        }
        // Every set the state no longer names goes, found on SYSTEM rather
        // than in the old state, so a finish interrupted here removes them
        // when it runs again.
        let named = super::sets::named_sets(&alone);
        let directory = set_state::kind_dir(&self.system, kind);
        let mut unnamed = Vec::new();
        let listed = directory.symlink_metadata().is_ok();
        for entry in listed
            .then(|| std::fs::read_dir(&directory))
            .transpose()?
            .into_iter()
            .flatten()
        {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json"))
                && valid_id(id).is_ok()
                && !named.contains(id)
                && entry.file_type()?.is_file()
            {
                unnamed.push(entry.path());
            }
        }
        if !unnamed.is_empty() {
            for path in unnamed {
                std::fs::remove_file(path)?;
            }
            sync_dir(&directory)?;
        }
        removed += self.release_unreferenced(kind, keys)?;
        removed += self.remove_all_extensions()?;
        Ok(removed)
    }
}
