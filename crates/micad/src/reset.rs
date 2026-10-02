//! Reset tier execution: the applier of a staged reset.
//!
//! **This module is the second half of a two-writer operation.** apid checks
//! the authority of each tier and commits ONE record — `reset` in the
//! settings tree — and this module carries that record out on the next boot
//! and clears it. That is the shape: "a reset is
//! staged as an intent record plus an idempotent apply, never as a sequence
//! whose interruption is a third state". A power loss leaves the device either
//! pre-reset (no record, nothing happened) or mid-reset (record still staged),
//! and the next boot replays the same tier until the record is gone.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use micad_settings::{ProvisioningState, ResetTier, Settings, Store};

/// The DATA pool root, and the variable that relocates it.
pub const DATA_ROOT_ENV: &str = "MICA_DATA_ROOT";
/// Where the pool is mounted when nothing relocates it (`/mica` and `/srv` are binds
/// of this ONE pool).
pub const DEFAULT_DATA_ROOT: &str = "/mnt/data";

/// The system-owned DATA namespace, relative to the pool root.
const SYSTEM_DIR: &str = "mica";
/// The user-owned DATA namespace, relative to the pool root.
const USER_DIR: &str = "srv";

/// The `/mica` skeleton `mica-data-layout` establishes on a virgin device, in
/// its own order.
const SYSTEM_SKELETON: &[&str] = &[
    "ui",
    "config",
    "containers",
    "diagnostics",
    "home",
    "root",
    "apps",
    "updates",
    "updates/downloads",
    "updates/verified",
    "updates/staging",
];

/// The system configuration namespace, relative to `/mica`.
const CONFIG_DIR: &str = "config";

/// The `/mica` subtrees the application layer owns, which tier 2 clears.
///
/// footnote `[^apps-mica]`: `/mica` is the system-owned namespace and tier
/// 2 does not empty it. `ui/`, `updates/` — an acquired deployment is not
/// application data — and the `home/`/`root/` backing directories are not
/// opened.
const APPLICATION_DIRS: &[&str] = &["apps"];

/// The console's TLS identity on STATE, relative to it. The configuration
/// tiers remove it: an uploaded key or a certificate issued to the operator's
/// names is their configuration, and apid generates a fresh self-signed one
/// the next time it serves HTTPS.
const APID_IDENTITY: &str = "apid/identity.pem";

/// The STATE directories holding application enrolment records.
///
/// `containerd` is mica-containerd's store of declarations: cleared with the
/// settings that declared them, or the daemon would bring the old containers
/// back at boot before micad declares none. `quadlet` holds Quadlet files,
/// which nothing reads.
const STATE_APPLICATION_DIRS: &[&str] = &["containerd", "quadlet", "systemd-units"];

/// The roots a tier is allowed to reach. Nothing outside them is opened.
#[derive(Debug, Clone)]
pub struct Roots {
    /// The physical DATA filesystem root.
    pub data: PathBuf,
    /// The physical DATA/state namespace.
    pub state: PathBuf,
}

impl Roots {
    /// Resolve one physical DATA root, including its persistent state.
    #[must_use]
    pub fn from_env() -> Self {
        let data = std::env::var_os(DATA_ROOT_ENV)
            .map_or_else(|| PathBuf::from(DEFAULT_DATA_ROOT), PathBuf::from);
        Self {
            state: data.join("state"),
            data,
        }
    }

    fn system(&self) -> PathBuf {
        self.data.join(SYSTEM_DIR)
    }

    fn containers(&self) -> PathBuf {
        self.data.join("containers")
    }

    fn user(&self) -> PathBuf {
        self.data.join(USER_DIR)
    }
}

/// Traversal is bounded before any removal and again during the mutation pass.
struct Traversal {
    started: std::time::Instant,
    entries: usize,
}
impl Traversal {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            entries: 0,
        }
    }
    fn visit(&mut self, depth: usize) -> Result<()> {
        anyhow::ensure!(depth <= 64, "reset directory depth exceeds limit");
        self.entries += 1;
        anyhow::ensure!(self.entries <= 1_000_000, "reset entry count exceeds limit");
        anyhow::ensure!(
            self.started.elapsed() < std::time::Duration::from_secs(30),
            "reset traversal deadline exceeded"
        );
        Ok(())
    }
}

fn preflight_tree(path: &Path, depth: usize, traversal: &mut Traversal) -> Result<()> {
    traversal.visit(depth)?;
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    // Symlinks are removable leaves, never traversal roots.
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            preflight_tree(&entry?.path(), depth + 1, traversal)?;
        }
    }
    Ok(())
}

fn preflight_reset(roots: &Roots, tier: ResetTier) -> Result<()> {
    let targets = match tier {
        ResetTier::Configuration => vec![roots.system().join(CONFIG_DIR)],
        ResetTier::ApplicationData => APPLICATION_DIRS
            .iter()
            .map(|dir| roots.system().join(dir))
            .chain(
                STATE_APPLICATION_DIRS
                    .iter()
                    .map(|dir| roots.state.join(dir)),
            )
            .chain([roots.user(), roots.containers()])
            .collect(),
        ResetTier::FullFactory => STATE_APPLICATION_DIRS
            .iter()
            .map(|dir| roots.state.join(dir))
            .chain([roots.system(), roots.user(), roots.containers()])
            .collect(),
    };
    let mut traversal = Traversal::new();
    for target in targets {
        preflight_tree(&target, 0, &mut traversal)?;
    }
    Ok(())
}

/// What [`apply_pending`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No intent was staged; the boot carries on untouched.
    NoIntent,
    /// The staged tier was applied and the record cleared.
    Applied(ResetTier),
}

/// Apply the staged reset intent, if there is one, and clear it.
pub fn apply_pending(store: &Store, settings: &mut Settings, roots: &Roots) -> Result<Outcome> {
    let Some(intent) = settings.reset.clone() else {
        return Ok(Outcome::NoIntent);
    };
    validate_roots(roots)?;
    let lock_path = roots.data.join("meta/transaction.lock");
    if let Ok(metadata) = lock_path.symlink_metadata() {
        anyhow::ensure!(metadata.is_file(), "invalid storage transaction lock");
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.try_lock()
        .context("another storage transaction is active")?;
    preflight_reset(roots, intent.tier)?;
    tracing::info!(
        tier = ?intent.tier,
        requested = intent.requested,
        presence = intent.presence.as_deref().unwrap_or("none"),
        "applying a staged reset"
    );

    match intent.tier {
        ResetTier::Configuration => {
            // The only DATA store tier 1 opens, and it opens the whole of it.
            // What was expensive at a file granularity -- teaching this
            // module to reach one document inside a transactional workspace --
            // is ordinary at a directory granularity, and it is the same
            // operation tier 2 already performs on `apps/` and `containers/`.
            let config = roots.system().join(CONFIG_DIR);
            clear_contents(&config).with_context(|| format!("clear {}", config.display()))?;
        }
        ResetTier::ApplicationData => clear_application_state(roots)?,
        ResetTier::FullFactory => {
            clear_application_state(roots)?;
            // `config` is in SYSTEM_SKELETON, so the re-seed empties it with
            // every other declared directory: tier 3 needs no clause of its
            // own for the namespace.
            reseed_tree(&roots.system(), SYSTEM_SKELETON)
                .with_context(|| format!("re-seed {}", roots.system().display()))?;
        }
    }

    if matches!(
        intent.tier,
        ResetTier::Configuration | ResetTier::FullFactory
    ) {
        let identity = roots.state.join(APID_IDENTITY);
        match fs::remove_file(&identity) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("remove {}", identity.display()));
            }
        }
    }

    // The commit. Built from the tree in hand and saved once; `*settings` is
    // replaced only after the save has returned, so no caller observes a tree
    // that is not on STATE.
    let mut applied = match intent.tier {
        ResetTier::Configuration | ResetTier::FullFactory => {
            reseeded_settings(settings, intent.tier)
        }
        // Tier 2 keeps the operator's configuration and takes their
        // applications. A declared container IS an operator application --
        // the entry is what micad declares to mica-containerd and what brings
        // the workload back -- so clearing the data under `/mica/` without clearing the
        // declarations would leave a reset device running the same workload
        // against an empty volume.
        ResetTier::ApplicationData => Settings {
            container: micad_settings::ContainerSettings {
                units: std::collections::BTreeMap::new(),
                ..settings.container.clone()
            },
            // A paired device is operator data too: the phone in somebody's
            // pocket is not this appliance's configuration, and a reset that
            // kept the trust list would hand the next operator a device that
            // still reconnects to the last one's.
            bluetooth: micad_settings::BluetoothSettings {
                devices: std::collections::BTreeMap::new(),
                ..settings.bluetooth.clone()
            },
            ..settings.clone()
        },
    };
    applied.reset = None;
    store
        .save(&applied)
        .context("persist the tree the reset produced")?;
    *settings = applied;

    tracing::info!(tier = ?intent.tier, "reset applied");
    Ok(Outcome::Applied(intent.tier))
}

/// The tree a settings-clearing tier leaves behind.
fn reseeded_settings(before: &Settings, tier: ResetTier) -> Settings {
    let mut after = Settings {
        // The identity record, PRESERVED by both tiers (footnote
        // `[^identity]`), with `state` put back so first-boot provisioning
        // re-seeds the hostname from it and the SSH default.
        provisioning: micad_settings::ProvisioningSettings {
            state: ProvisioningState::Pending,
            ..before.provisioning.clone()
        },
        ..Settings::default()
    };
    // The per-device credential and its generation: a first-boot secret, not a
    // management credential, and `identity::ensure_identity` would re-mint it
    // if it were dropped. the `identity: preserved` covers it on both tiers.
    after.access.device = before.access.device.clone();

    if tier == ResetTier::Configuration {
        // footnote `[^cfg]`: tier 1 keeps the apid management credential.
        // A configuration reset that dropped it would be a lockout dressed as
        // a settings action, and a remote credential-clearing primitive —
        // credential recovery is where a credential is deliberately replaced, under the authority and nowhere else.
        after.access.web_admin = before.access.web_admin.clone();
        after.access.claim = before.access.claim;
        after.access.api_tokens = before.access.api_tokens.clone();
    }
    after
}

/// Tier 2's whole scope, and tier 3's share of it: the application layer's
/// enrolment records on STATE, its subtrees under `/mica`, and `/srv`.
fn clear_application_state(roots: &Roots) -> Result<()> {
    for dir in STATE_APPLICATION_DIRS {
        let path = roots.state.join(dir);
        clear_contents(&path).with_context(|| format!("clear {}", path.display()))?;
    }
    for dir in APPLICATION_DIRS {
        let path = roots.system().join(dir);
        clear_contents(&path).with_context(|| format!("clear {}", path.display()))?;
    }
    reseed_tree(&roots.containers(), &["networks", "tmp"])?;
    // `/srv` is `cleared` and not `re-seeded`:
    // the product gives that namespace to the operator, so mica recreates the
    // mount point and never its contents. Clearing the contents and keeping
    // the directory is exactly that.
    let user = roots.user();
    clear_contents(&user).with_context(|| format!("clear {}", user.display()))
}

/// Validate physical namespaces before the first removal. A same-device check
/// cannot detect a bind into another DATA namespace, so inspect mount points.
fn validate_roots(roots: &Roots) -> Result<()> {
    anyhow::ensure!(roots.data.is_absolute(), "DATA root must be absolute");
    anyhow::ensure!(
        roots.state == roots.data.join("state"),
        "state must be on DATA"
    );
    for path in [
        roots.data.clone(),
        roots.state.clone(),
        roots.data.join("meta"),
        roots.system(),
        roots.user(),
        roots.containers(),
    ] {
        anyhow::ensure!(
            path.symlink_metadata()?.is_dir(),
            "invalid physical directory {}",
            path.display()
        );
    }
    let mounts = fs::read_to_string("/proc/self/mountinfo")?;
    reject_nested_mounts(&roots.data, &mounts)?;
    for path in STATE_APPLICATION_DIRS
        .iter()
        .map(|p| roots.state.join(p))
        .chain(SYSTEM_SKELETON.iter().map(|p| roots.system().join(p)))
        .chain([
            roots.containers().join("networks"),
            roots.containers().join("tmp"),
        ])
    {
        if let Ok(metadata) = path.symlink_metadata() {
            anyhow::ensure!(
                metadata.is_dir(),
                "invalid reset namespace {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn reject_nested_mounts(data: &Path, mounts: &str) -> Result<()> {
    for line in mounts.lines() {
        let Some(path) = line.split_whitespace().nth(4) else {
            continue;
        };
        let path = path
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let path = Path::new(&path);
        anyhow::ensure!(
            !path.starts_with(data) || path == data,
            "unexpected mount inside reset backing storage: {}",
            path.display()
        );
    }

    Ok(())
}

/// Remove every entry inside `dir`, keeping `dir` itself.
fn clear_contents(dir: &Path) -> Result<()> {
    clear_directory(dir, 0, &mut Traversal::new())
}

fn clear_directory(dir: &Path, depth: usize, traversal: &mut Traversal) -> Result<()> {
    traversal.visit(depth)?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry of {}", dir.display()))?;
        remove(&entry.path(), &entry.file_type()?, depth + 1, traversal)?;
    }
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Reduce `root` to exactly `skeleton`, with every directory in it empty.
///
/// The declared paths are kept and descended into; everything else is removed.
/// Descending rather than deleting the whole tree keeps the modes
/// `mica-data-layout` established, so this module never restates them.
fn reseed_tree(root: &Path, skeleton: &[&str]) -> Result<()> {
    let declared: BTreeSet<PathBuf> = skeleton.iter().map(PathBuf::from).collect();
    reseed_dir(root, Path::new(""), &declared, 0, &mut Traversal::new())
}

fn reseed_dir(
    dir: &Path,
    relative: &Path,
    declared: &BTreeSet<PathBuf>,
    depth: usize,
    traversal: &mut Traversal,
) -> Result<()> {
    traversal.visit(depth)?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry of {}", dir.display()))?;
        let file_type = entry.file_type()?;
        let child = relative.join(entry.file_name());
        // A declared path is kept only when it is a real directory: a file or
        // a symlink standing where a skeleton directory belongs is not that
        // directory, and leaving it would leave the pretence of a re-seeded
        // tree.
        if file_type.is_dir() && declared.contains(&child) {
            reseed_dir(&entry.path(), &child, declared, depth + 1, traversal)?;
        } else {
            remove(&entry.path(), &file_type, depth + 1, traversal)?;
        }
    }
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn remove(
    path: &Path,
    file_type: &fs::FileType,
    depth: usize,
    traversal: &mut Traversal,
) -> Result<()> {
    traversal.visit(depth)?;
    let removed = if file_type.is_dir() {
        clear_directory(path, depth, traversal)?;
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests;
