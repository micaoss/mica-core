//! Signed deployment acquisition, operator policy and boot lifecycle.
//! Native commands enforce trust, capacity and generation; this layer records
//! progress and applies the shared maintenance and reboot gates.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::update_codes::CodedReason;
use crate::update_policy::PolicyStore;

mod acquisition;
mod client;
mod outcomes;
mod policy;
mod snapshot;
pub use client::*;
pub use outcomes::*;
use policy::*;

/// Fixed native command shipped in the complete system image.
pub const DEFAULT_CLIENT_PATH: &str = "/usr/bin/mica-deploy";

/// The `/mica/updates` workspace the client acquires into:
/// partials in `downloads/`, verified descriptors and objects in `verified/`. The client's
/// own default; restated here because this module decides what may be
/// recorded as `ready` and what may be installed.
pub const DEFAULT_WORKSPACE_ROOT: &str = "/mica/updates";

/// Bound on one `probe` subprocess: a handful of stat calls and one fsync;
/// a minute is a wedged disk, not a slow one.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on one `check` subprocess: metadata is a handful of files
/// capped at 1 MiB each, so ten minutes is generous slack for a slow link,
/// not an expected duration.
const CHECK_TIMEOUT: Duration = Duration::from_secs(600);

/// Bound on one `fetch` subprocess. Component payloads can be hundreds of MiB and the link
/// may be slow; four hours is a fault bound, and a fetch that dies there
/// resumes from its `.part` file on the next attempt.
const FETCH_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

/// What the bus layer gives the lifecycle to observe and record with.
#[async_trait::async_trait]
pub trait LifecycleHost: Send + Sync {
    /// Replace the live-state `update.lifecycle` entry with `lifecycle`.
    async fn record(&self, lifecycle: Value);
    /// The live-state `health` subtree as last reported (empty object when
    /// nothing has reported).
    async fn health(&self) -> Value;
}

/// A refused action, split by what the caller can do about it.
#[derive(Debug)]
pub enum Refusal {
    /// The policy forbids it right now; the message names the rule.
    Policy(String),
    /// Another lifecycle operation or an install is in flight.
    Busy(String),
    /// The client binary is not there to run.
    Unavailable(String),
    /// The request itself is malformed (e.g. an override TTL of zero).
    Invalid(String),
}

impl Refusal {
    pub fn message(&self) -> &str {
        match self {
            Self::Policy(m) | Self::Busy(m) | Self::Unavailable(m) | Self::Invalid(m) => m,
        }
    }
}

/// Why the last automatic pass did not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Deferral {
    /// The vocabulary: `outside-window`,
    /// `reboot-gate-closed`, `clock-untrusted`, `version-suppressed`, plus
    /// the driver's own guards — resolved through
    /// [`crate::update_codes::deferral_code`], so this field holds a code and
    /// can hold nothing else.
    reason: &'static str,
    /// The refusing rule in words, verbatim from whoever refused.
    detail: String,
    /// When this reason first applied without interruption.
    since: DateTime<Utc>,
    /// The most recent attempt it applied to.
    at: DateTime<Utc>,
    /// How many attempts in a row it has applied to.
    count: u64,
}

/// An active administrative override of the reboot gate.
#[derive(Debug, Clone)]
struct OverrideRecord {
    expires_at: DateTime<Utc>,
    requested_by: String,
}

/// The machine the mutex guards: every recorded fact that is not read live.
#[derive(Default)]
struct Machine {
    /// `Some("checking")` / `Some("downloading")` while a client subprocess
    /// runs; the busy guard and the reported state in one field.
    operation: Option<&'static str>,
    /// Why the last operation failed, with its code; cleared when the next
    /// one starts.
    failed: Option<CodedReason>,
    /// The workspace's refusal, when the last probe did not pass; cleared
    /// when the next operation starts and stays clear when its probe passes.
    unready: Option<Unready>,
    /// The last passing probe's report (`pool`, `free`, `used`, ...).
    workspace: Option<Value>,
    available: Option<Available>,
    /// The verified descriptor path the last fetch staged.
    descriptor: Option<String>,
    last_check: Option<String>,
    boot_phase: Option<(&'static str, String)>,
    override_record: Option<OverrideRecord>,
    /// Why the last automatic attempt did not proceed; `None` once one did.
    deferred: Option<Deferral>,
}

/// The lifecycle service: one per daemon, shared with the auto-check task.
pub struct UpdateLifecycle {
    client: Arc<dyn UpdateClient>,
    policy: PolicyStore,
    host: Arc<dyn LifecycleHost>,
    /// The bus layer's install-in-flight flag, shared so `installing` here
    /// and the install refusal there can never disagree.
    installing: Arc<AtomicBool>,
    /// The `/mica/updates` workspace root; `verified/` below it is the only
    /// place a recorded or installed descriptor may be.
    workspace_root: PathBuf,
    machine: Mutex<Machine>,
}

impl UpdateLifecycle {
    pub fn new(
        client: Arc<dyn UpdateClient>,
        policy: PolicyStore,
        host: Arc<dyn LifecycleHost>,
        installing: Arc<AtomicBool>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            client,
            policy,
            host,
            installing,
            workspace_root,
            machine: Mutex::new(Machine::default()),
        }
    }

    /// The client this lifecycle runs, for a rebuild around a new workspace.
    #[cfg(test)]
    pub fn client(&self) -> Arc<dyn UpdateClient> {
        Arc::clone(&self.client)
    }

    /// The policy store, for the same rebuild.
    pub fn policy(&self) -> PolicyStore {
        self.policy.clone()
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// `<workspace>/verified`: the one directory an installable descriptor is in.
    pub fn verified_dir(&self) -> PathBuf {
        self.workspace_root.join("verified")
    }

    /// Where an uploaded archive is streamed to before it is imported.
    ///
    /// Inside the acquisition workspace and beside `verified/`, so an upload
    /// lands on the same medium the objects it carries will be staged onto:
    /// a device with no room for the deployment runs out of it while writing
    /// the upload, which is the earlier and cheaper failure.
    #[must_use]
    pub fn uploads_dir(&self) -> PathBuf {
        self.workspace_root.join("uploads")
    }

    /// Why `path` must not be handed to the native backend, or `Ok` when it is a regular
    /// file (not a symbolic link) directly inside `verified/` and not a
    /// `.part`. The bus layer's `InstallUpdate` asks this for every path,
    /// including an operator's explicit one: a partial, or a file outside
    /// `verified/`, is never installable by filename alone.
    pub fn installable(&self, path: &Path) -> Result<(), String> {
        let verified = self.verified_dir();
        if !path.is_absolute() || path.parent() != Some(verified.as_path()) {
            return Err(format!(
                "descriptor path `{}` is not inside the verified directory {}",
                path.display(),
                verified.display()
            ));
        }
        if crate::deployment::staged_id(path).is_none() {
            return Err(format!(
                "path `{}` is not a deployment descriptor or a core set",
                path.display()
            ));
        }
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_file() => Ok(()),
            Ok(_) => Err(format!(
                "descriptor path `{}` is not a regular file (a symbolic link is not followed)",
                path.display()
            )),
            Err(err) => Err(format!("descriptor path `{}`: {err}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests;
