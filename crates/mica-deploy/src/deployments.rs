//! Durable file-deployment state and native UEFI/FIT boot records.
use crate::boot::selected_entry;
use crate::fit_env::{Environment, FitLayout};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

mod install;
mod io;
mod lifecycle;
pub use install::*;
pub use io::*;
pub use mica_fs::{read_bounded, sync_dir};

/// Shared durable metadata cannot be read or committed. Selecting another
/// deployment cannot repair DATA, so boot health must enter recovery.
#[derive(Debug)]
pub struct SharedDataFailure;
impl std::fmt::Display for SharedDataFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shared DATA metadata unavailable; recovery required")
    }
}
impl std::error::Error for SharedDataFailure {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BootReceipt {
    pub backend: crate::boot::BootKind,
    pub deployment_id: String,
    pub entry: String,
    pub kernel_id: String,
    pub rootfs_id: String,
    pub content_verified: bool,
    pub secure_boot: bool,
    pub boot_verified: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct State {
    pub highest_generation: u64,
    pub current: Option<String>,
    pub fallback: Option<String>,
    pub candidate: Option<String>,
    pub failed: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub file: String,
    pub generation: u64,
    pub tries_left: Option<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatus {
    #[serde(flatten)]
    pub entry: Entry,
    pub version: String,
    pub kernel_id: String,
    pub kernel_release: String,
    pub rootfs_id: String,
}

pub enum BootBackend {
    Uefi {
        esp: PathBuf,
    },
    Fit {
        firmware: PathBuf,
        layout: FitLayout,
    },
}

pub struct DeploymentStore {
    pub system: PathBuf,
    pub boot: BootBackend,
    pub meta: PathBuf,
    /// What installation keeps free on SYSTEM and the ESP, and a download on
    /// DATA: the signed boot policy's, or the defaults.
    pub reserves: crate::board::Reserves,
}

/// The device a deployment must name: the board and architecture of the signed
/// boot policy and the product of the running root.
pub struct Target<'a> {
    pub board: &'a str,
    pub arch: &'a str,
    pub product: &'a str,
}

impl DeploymentStore {
    pub fn new(system: PathBuf, boot: BootBackend, meta: PathBuf) -> Self {
        Self {
            system,
            boot,
            meta,
            reserves: crate::board::Reserves::default(),
        }
    }

    /// Keep the reserves the board declares.
    #[must_use]
    pub fn with_reserves(mut self, reserves: crate::board::Reserves) -> Self {
        self.reserves = reserves;
        self
    }

    /// Hold this across installation, confirmation, reset or garbage collection.
    pub fn lock(&self) -> Result<File> {
        let file = (|| -> Result<File> {
            ensure!(
                self.meta.symlink_metadata()?.is_dir(),
                "metadata namespace is not a directory"
            );
            let path = self.meta.join("transaction.lock");
            if path.symlink_metadata().is_ok() {
                ensure!(path.symlink_metadata()?.is_file(), "invalid lock file");
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            Ok(file)
        })()
        .context(SharedDataFailure)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .context("another storage transaction is active")?;
        Ok(file)
    }

    pub fn state(&self) -> Result<State> {
        (|| -> Result<State> {
            let path = self.meta.join("deployments.json");
            if !path.try_exists()? && path.symlink_metadata().is_err() {
                return Ok(State::default());
            }
            let state: State = serde_json::from_slice(&read_bounded(&path, 24576)?)?;
            ensure!(
                state.failed.len() <= 128,
                "failed deployment set exceeds limit"
            );
            for id in [&state.current, &state.fallback, &state.candidate]
                .into_iter()
                .flatten()
                .chain(state.failed.iter())
            {
                valid_id(id)?;
            }
            Ok(state)
        })()
        .context(SharedDataFailure)
    }

    pub fn save_state(&self, state: &State) -> Result<()> {
        atomic_write(
            &self.meta.join("deployments.json"),
            &serde_json::to_vec(state)?,
        )
        .context(SharedDataFailure)
    }

    /// Native boot entries commit activation before DATA's diagnostic state.
    /// Reconcile that crash window without refilling any boot attempts.
    pub fn effective_state(&self) -> Result<State> {
        let mut state = self.state()?;
        let entries = self.entries()?;
        state.highest_generation = state.highest_generation.max(
            entries
                .iter()
                .map(|entry| entry.generation)
                .max()
                .unwrap_or(0),
        );
        if let Some(confirmed) = entries
            .iter()
            .find(|entry| entry.tries_left.is_none() && !state.failed.contains(&entry.id))
            && state.current.as_deref() != Some(confirmed.id.as_str())
        {
            let usable = |id: &String| {
                entries.iter().any(|entry| {
                    &entry.id == id
                        && entry.id != confirmed.id
                        && entry.tries_left != Some(0)
                        && !state.failed.contains(id)
                })
            };
            state.fallback = state
                .current
                .clone()
                .filter(usable)
                .or_else(|| state.fallback.clone().filter(usable));
            state.current = Some(confirmed.id.clone());
        }
        // Retirement commits in the native records before DATA. An absent
        // fallback must never protect objects after that commit.
        state.fallback = state
            .fallback
            .filter(|id| entries.iter().any(|e| &e.id == id));
        if let Some(current) = &state.current {
            let generation = entries
                .iter()
                .find(|entry| &entry.id == current)
                .context("confirmed deployment entry is missing")?
                .generation;
            let candidates: Vec<_> = entries
                .iter()
                .filter(|entry| {
                    entry.generation > generation
                        && entry.tries_left.is_some_and(|tries| tries > 0)
                        && !state.failed.contains(&entry.id)
                        && state.fallback.as_deref() != Some(entry.id.as_str())
                })
                .collect();
            ensure!(candidates.len() <= 1, "ambiguous pending deployments");
            state.candidate = candidates.first().map(|entry| entry.id.clone());
        }
        Ok(state)
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        let esp = match &self.boot {
            BootBackend::Uefi { esp } => esp,
            BootBackend::Fit { firmware, layout } => {
                return Ok(Environment::load(firmware, *layout)?
                    .records
                    .into_iter()
                    .map(|r| Entry {
                        file: format!("fit:{}", r.id),
                        id: r.id,
                        generation: r.generation,
                        tries_left: r.tries_left,
                    })
                    .collect());
            }
        };
        let directory = esp.join("loader/entries");
        ensure!(
            directory.symlink_metadata()?.is_dir(),
            "invalid entries directory"
        );
        let mut entries: Vec<Entry> = Vec::new();
        for item in fs::read_dir(directory)? {
            let item = item?;
            let file = item
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid entry name"))?;
            if !file.starts_with("mica-") || !file.ends_with(".conf") {
                continue;
            }
            let id = selected_entry(&file)?;
            ensure!(
                !entries.iter().any(|entry| entry.id == id),
                "ambiguous boot entries for {id}"
            );
            let content = String::from_utf8(read_bounded(&item.path(), 4096)?)?;
            let versions = content
                .lines()
                .filter_map(|line| line.strip_prefix("version "))
                .collect::<Vec<_>>();
            ensure!(versions.len() == 1, "entry has no unique version");
            let generation: u64 = versions[0].parse()?;
            ensure!(
                generation > 0 && generation.to_string() == versions[0],
                "invalid entry generation"
            );
            let tries_left = file
                .split_once('+')
                .map(|(_, count)| count.as_bytes()[0] - b'0');
            entries.push(Entry {
                id,
                file,
                generation,
                tries_left,
            });
            ensure!(entries.len() <= 2, "too many deployment entries");
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.generation));
        Ok(entries)
    }

    fn validate_receipt(&self, receipt: &BootReceipt) -> Result<()> {
        ensure!(
            receipt.content_verified,
            "running content is not authenticated"
        );
        valid_id(&receipt.deployment_id)?;
        match &self.boot {
            BootBackend::Uefi { .. } => {
                ensure!(
                    receipt.backend == crate::boot::BootKind::Uefi
                        && receipt.boot_verified == receipt.secure_boot,
                    "boot receipt backend mismatch"
                );
                ensure!(
                    selected_entry(&receipt.entry)? == receipt.deployment_id,
                    "boot receipt identity mismatch"
                );
            }
            BootBackend::Fit { firmware, layout } => {
                ensure!(
                    receipt.backend == crate::boot::BootKind::UbootFit
                        && !receipt.secure_boot
                        && receipt.boot_verified,
                    "boot receipt backend mismatch"
                );
                ensure!(
                    receipt.entry == format!("fit:{}", receipt.deployment_id),
                    "boot receipt identity mismatch"
                );
                let env = Environment::load(firmware, *layout)?;
                ensure!(
                    env.records
                        .iter()
                        .any(|r| r.id == receipt.deployment_id && r.kernel_id == receipt.kernel_id),
                    "boot receipt kernel mismatch"
                );
            }
        }
        Ok(())
    }

    fn set_tries(&self, entry: &Entry, tries_left: Option<u8>) -> Result<()> {
        match &self.boot {
            BootBackend::Uefi { esp } => {
                let suffix = match tries_left {
                    None => ".conf",
                    Some(0) => "+0-3.conf",
                    _ => anyhow::bail!("attempts cannot be refilled"),
                };
                let directory = esp.join("loader/entries");
                fs::rename(
                    directory.join(&entry.file),
                    directory.join(format!("mica-{}{suffix}", entry.id)),
                )?;
                Ok(sync_dir(&directory)?)
            }
            BootBackend::Fit { firmware, layout } => {
                ensure!(
                    tries_left.is_none() || tries_left == Some(0),
                    "attempts cannot be refilled"
                );
                let mut env = Environment::load(firmware, *layout)?;
                let record = env
                    .records
                    .iter_mut()
                    .find(|r| r.id == entry.id && r.generation == entry.generation)
                    .context("boot record is missing")?;
                record.tries_left = tries_left;
                env.save(firmware)
            }
        }
    }

    /// Early PID 1, or a caller holding the DATA transaction lock, may retire a
    /// failed confirmed boot. Native trial counters remain firmware-owned.
    pub fn retire_failed_confirmed(&self, id: &str) -> Result<bool> {
        valid_id(id)?;
        let entries = self.entries()?;
        let entry = entries
            .iter()
            .find(|entry| entry.id == id)
            .context("selected boot record is missing")?;
        if entry.tries_left.is_some() {
            return Ok(false);
        }
        ensure!(
            entries
                .iter()
                .any(|entry| entry.id != id && entry.tries_left != Some(0)),
            "no usable fallback remains; recovery required"
        );
        self.set_tries(entry, Some(0))?;
        Ok(true)
    }

    pub fn fail_boot(&self, receipt: &BootReceipt) -> Result<bool> {
        self.validate_receipt(receipt)?;
        self.retire_failed_confirmed(&receipt.deployment_id)
    }

    fn retain_entries(&self, entries: &[Entry], keep: &[Option<&str>]) -> Result<()> {
        match &self.boot {
            BootBackend::Uefi { esp } => {
                for entry in entries {
                    if !keep.contains(&Some(entry.id.as_str())) {
                        fs::remove_file(esp.join("loader/entries").join(&entry.file))?;
                    }
                }
                Ok(sync_dir(&esp.join("loader/entries"))?)
            }
            BootBackend::Fit { firmware, layout } => {
                let mut env = Environment::load(firmware, *layout)?;
                env.records.retain(|r| keep.contains(&Some(r.id.as_str())));
                env.save(firmware)?;
                // Both redundant copies must forget retired records before
                // collection can remove their objects, including after retry.
                Environment::load(firmware, *layout)?.save(firmware)
            }
        }
    }
}
