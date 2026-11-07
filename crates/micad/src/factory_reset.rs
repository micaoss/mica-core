//! The factory reset, micad's half: the checks before it, the request it
//! stages for the runkit, and the finish, the record and the discard on the
//! first boot after it.
//!
//! The reset itself is the runkit's (`mica_deploy::factory_reset`): it runs
//! at the next boot, before anything of the root. A runkit of version 1
//! erases DATA in place and keeps its `meta/`; one of version 2 makes DATA
//! again, mica-deploy staging the request on SYSTEM and finishing the reset
//! there. The software stays at its version; everything configured goes.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mica_deploy::factory_reset::{self as erase, Done, Request, Requester};
use serde_json::{Value, json};

/// Where micad keeps the last reset it has recorded, relative to STATE. It
/// goes with STATE at the reset, so the first boot after one finds it absent.
const RECORD: &str = "mica/factory-reset.json";
/// Wrong passwords tolerated before micad's own backoff starts.
const FREE_FAILURES: u32 = 3;
/// The longest backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Why a factory reset was refused; `code` is the API's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The factory reset's paths and micad's backoff on wrong passwords.
pub struct FactoryReset {
    /// The DATA root: `meta/` holds the request and the runkit's record.
    data: PathBuf,
    /// STATE, where micad's own record goes.
    state: PathBuf,
    /// What the booted runkit says it does (`mica_deploy::factory_reset::CAPABILITIES`).
    capabilities: PathBuf,
    /// mica-deploy, which stages and finishes a reset of version 2.
    deploy: PathBuf,
    /// What the runkit did at this boot (`mica_deploy::factory_reset::PROGRESS`).
    progress: PathBuf,
    backoff: Mutex<Backoff>,
}

#[derive(Default)]
struct Backoff {
    failures: u32,
    until: Option<Instant>,
}

impl FactoryReset {
    #[must_use]
    pub fn new(data: PathBuf, state: PathBuf, capabilities: PathBuf) -> Self {
        Self {
            data,
            state,
            capabilities,
            deploy: PathBuf::from("/usr/bin/mica-deploy"),
            progress: PathBuf::from(erase::PROGRESS),
            backoff: Mutex::default(),
        }
    }

    /// Run `deploy` as mica-deploy, and read the runkit's account of this
    /// boot at `progress`.
    #[must_use]
    pub fn with_deploy(mut self, deploy: PathBuf, progress: PathBuf) -> Self {
        self.deploy = deploy;
        self.progress = progress;
        self
    }

    /// The device's: DATA at `/mnt/data` (or `MICA_DATA_ROOT`), STATE under it.
    #[must_use]
    pub fn production() -> Self {
        let roots = crate::reset::Roots::from_env();
        Self::new(roots.data, roots.state, PathBuf::from(erase::CAPABILITIES))
    }

    fn meta(&self) -> PathBuf {
        self.data.join("meta")
    }

    /// Whether the booted runkit carries out a staged reset.
    #[must_use]
    pub fn supported(&self) -> bool {
        self.version().is_some()
    }

    /// The version of the reset the booted runkit carries out: 1 erases DATA
    /// in place, 2 makes it again and returns the system to the running
    /// deployment.
    fn version(&self) -> Option<u64> {
        let value = mica_fs::read_bounded(&self.capabilities, 4096)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())?;
        (value["factoryReset"] == json!(true))
            .then(|| value["factoryResetVersion"].as_u64().unwrap_or(1))
    }

    /// Run mica-deploy with `args`, which must succeed.
    fn deploy(&self, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new(&self.deploy)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("run {}", self.deploy.display()))?;
        anyhow::ensure!(
            output.status.success(),
            "mica-deploy {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Finish, on the first start after the runkit carried it out, a reset of
    /// version 2: mica-deploy leaves the running deployment and its core set
    /// alone on SYSTEM and writes the record [`Self::first_boot`] reads. The
    /// boot carries nothing else of the reset.
    ///
    /// # Errors
    ///
    /// mica-deploy fails; the next command it runs finishes the reset.
    pub fn finish(&self) -> Result<()> {
        if self.progress.symlink_metadata().is_err() {
            return Ok(());
        }
        let done = self.deploy(&["factory-reset", "--finish"])?;
        tracing::info!(done, "factory reset finished on SYSTEM");
        Ok(())
    }

    /// The refusal while micad's backoff runs.
    pub fn throttled(&self) -> Option<Refusal> {
        let backoff = self
            .backoff
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let left = backoff.until?.checked_duration_since(Instant::now())?;
        Some(Refusal::new(
            "login_throttled",
            format!(
                "too many wrong passwords; try again in {} s",
                left.as_secs().max(1)
            ),
        ))
    }

    /// Count a wrong password: after [`FREE_FAILURES`] each one doubles the
    /// wait, from 5 s up to [`MAX_BACKOFF`].
    pub fn failed(&self) {
        let mut backoff = self
            .backoff
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        backoff.failures = backoff.failures.saturating_add(1);
        if let Some(over) = backoff.failures.checked_sub(FREE_FAILURES + 1) {
            let wait = Duration::from_secs(5)
                .saturating_mul(1 << over.min(10))
                .min(MAX_BACKOFF);
            backoff.until = Some(Instant::now() + wait);
        }
    }

    /// A right password clears the backoff.
    pub fn succeeded(&self) {
        *self
            .backoff
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Backoff::default();
    }

    /// Whether an update transaction holds DATA's lock right now.
    #[must_use]
    pub fn transaction_held(&self) -> bool {
        let Ok(lock) = std::fs::File::open(self.meta().join("transaction.lock")) else {
            return false;
        };
        lock.try_lock().is_err()
    }

    /// Stage the reset for the next boot.
    ///
    /// # Errors
    ///
    /// The request cannot be written.
    pub fn stage(&self, by: Requester, source: &str, now: u64) -> Result<()> {
        if self.version().is_some_and(|version| version >= 2) {
            let by = match by {
                Requester::Console => "console",
                Requester::Api => "api",
                Requester::Cli => "cli",
            };
            self.deploy(&["factory-reset", "--by", by, "--source", source])?;
            return Ok(());
        }
        erase::request(
            &self.meta(),
            &Request {
                v: 1,
                requested: now,
                by,
                source: source.to_string(),
                deployment: None,
            },
        )
    }

    /// The last completed reset, as the runkit recorded it.
    #[must_use]
    pub fn last(&self) -> Option<Done> {
        erase::done(&self.meta()).unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "the factory reset record is unreadable");
            None
        })
    }

    /// On the first boot after a reset: log it and record it on STATE, once,
    /// and answer it so the caller discards the freed space. Every later boot
    /// finds the record and answers `None`.
    ///
    /// # Errors
    ///
    /// The record cannot be written.
    pub fn first_boot(&self) -> Result<Option<Done>> {
        let Some(done) = self.last() else {
            return Ok(None);
        };
        let record = self.state.join(RECORD);
        if mica_fs::read_bounded(&record, 4096)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Done>(&bytes).ok())
            .is_some_and(|recorded| recorded == done)
        {
            return Ok(None);
        }
        tracing::warn!(
            requested = done.requested,
            completed = done.completed,
            by = ?done.by,
            source = done.source,
            unremoved = done.unremoved,
            "factory reset completed"
        );
        if let Some(parent) = record.parent() {
            std::fs::create_dir_all(parent)?;
        }
        mica_fs::Replace::new(&record, "pending")?
            .mode(0o600)
            .write(serde_json::to_vec(&done)?)
            .with_context(|| format!("write {}", record.display()))?;
        Ok(Some(done))
    }

    /// Discard DATA's free space, best effort: a filesystem or a device that
    /// does not discard (UBIFS, a card without TRIM) is skipped.
    pub fn discard_free_space(&self) {
        let outcome = std::fs::File::open(&self.data)
            .map_err(anyhow::Error::from)
            .and_then(|dir| Ok(lifecycle_sys::trim_free_space(&dir)?));
        match outcome {
            Ok(bytes) => {
                tracing::info!(
                    bytes,
                    "freed space on DATA discarded after the factory reset"
                );
            }
            Err(error) => tracing::debug!(%error, "DATA's free space was not discarded"),
        }
    }
}

/// The answer of `GetFactoryReset`: whether the booted runkit supports it,
/// and the last completed reset.
#[must_use]
pub fn status(reset: &FactoryReset) -> Value {
    json!({
        "supported": reset.supported(),
        "last": reset.last(),
    })
}

/// The requester `by` names, as the bus carries it.
#[must_use]
pub fn requester(by: &str) -> Option<Requester> {
    match by {
        "console" => Some(Requester::Console),
        "api" => Some(Requester::Api),
        "cli" => Some(Requester::Cli),
        _ => None,
    }
}

/// The DATA root a test points the reset at.
#[cfg(test)]
pub(crate) fn at(root: &std::path::Path) -> FactoryReset {
    FactoryReset::new(
        root.join("data"),
        root.join("data/state"),
        root.join("run/runkit.json"),
    )
}

#[cfg(test)]
mod tests;
