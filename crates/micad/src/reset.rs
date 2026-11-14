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
//!
//! **No tier crosses into another mount.** A directory of a tree a tier
//! clears that something is mounted on -- the user's disk at an engine's root,
//! a bind -- is left in place with everything under it: the user's mounts are
//! the user's. What is on DATA itself is cleared as before.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
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
    "updates",
    "updates/downloads",
    "updates/verified",
    "updates/staging",
];

/// The system configuration namespace, relative to `/mica`.
const CONFIG_DIR: &str = "config";

/// The `/mica` subtrees the application layer owns, which tier 2 clears.
///
/// `/mica` is the system-owned namespace and tier 2 does not empty it. `ui/`, `updates/` — an acquired deployment is not
/// application data — and the `home/`/`root/` backing directories are not
/// opened.
///
/// `volumes` holds each Compose project's directory, its named volumes and
/// relative paths, which mica-containerd makes.
const APPLICATION_DIRS: &[&str] = &["volumes"];

/// The console's TLS identity on STATE, relative to it. The configuration
/// tiers remove it: an uploaded key or a certificate issued to the operator's
/// names is their configuration, and apid generates a fresh self-signed one
/// the next time it serves HTTPS.
const APID_IDENTITY: &str = "apid/identity.pem";
/// micad's record of the board defaults it added, relative to STATE. The
/// configuration tiers remove it with the settings, so the board's defaults
/// come back.
const BOARD_DEFAULTS_RECORD: &str = "mica/seeded-defaults.json";

/// The STATE directories holding application enrolment records.
///
/// `containerd` is mica-containerd's store of declarations: cleared with the
/// settings that declared them, or the daemon would bring the old containers
/// back at boot before micad declares none. `systemd-units` and `openrc-local`
/// hold the user's services (`services.units`), bound to
/// `/usr/local/lib/systemd/system` and `/usr/local/etc`. `accounts` holds the
/// service accounts mica-accounts keeps for those services, rebuilt after the
/// reset.
const STATE_APPLICATION_DIRS: &[&str] =
    &["containerd", "systemd-units", "openrc-local", "accounts"];
/// dockerd's pid file, written while it runs.
const DOCKER_PID_FILE: &str = "/run/docker.pid";

/// The roots a tier is allowed to reach. Nothing outside them is opened.
#[derive(Debug, Clone)]
pub struct Roots {
    /// The physical DATA filesystem root.
    pub data: PathBuf,
    /// The physical DATA/state namespace.
    pub state: PathBuf,
    /// dockerd's pid file. A running dockerd holds its storage under
    /// DATA/containers, so the tiers that clear it refuse to run beside one.
    pub docker_pid: PathBuf,
    /// The mount table the walks read the mounts on DATA from.
    pub mountinfo: PathBuf,
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
            docker_pid: PathBuf::from(DOCKER_PID_FILE),
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
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

/// One pass over the trees a tier clears: its bounds, and the directories of
/// DATA that something is mounted on, which it leaves in place.
struct Walk {
    traversal: Traversal,
    mounts: BTreeSet<PathBuf>,
}

impl Walk {
    fn new(mounts: &BTreeSet<PathBuf>) -> Self {
        Self {
            traversal: Traversal::new(),
            mounts: mounts.clone(),
        }
    }

    /// Whether the directory `path` is a mount, by the mount table or by a
    /// device other than its parent's (`parent_dev`); one is logged.
    fn is_mount(&self, path: &Path, metadata: &fs::Metadata, parent_dev: u64) -> bool {
        let mounted = self.mounts.contains(path) || metadata.dev() != parent_dev;
        if mounted {
            tracing::info!(
                path = %path.display(),
                "a mount in a tree the reset clears: left in place with its contents"
            );
        }
        mounted
    }

    /// Whether a mount lies below the directory `path`, which therefore stays.
    fn holds_mount(&self, path: &Path) -> bool {
        self.mounts
            .iter()
            .any(|mount| mount != path && mount.starts_with(path))
    }
}

/// The device of the directory holding `path`, for the mount check of a walk's
/// root.
fn parent_dev(path: &Path) -> Result<u64> {
    let parent = path.parent().unwrap_or(path);
    Ok(parent
        .symlink_metadata()
        .with_context(|| format!("read {}", parent.display()))?
        .dev())
}

/// Whether the walk's root `path` is itself a mount, which it leaves alone.
fn root_is_mount(path: &Path, walk: &Walk) -> Result<bool> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(metadata.is_dir() && walk.is_mount(path, &metadata, parent_dev(path)?))
}

fn preflight_tree(path: &Path, depth: usize, parent_dev: u64, walk: &mut Walk) -> Result<()> {
    walk.traversal.visit(depth)?;
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    // Symlinks are removable leaves, never traversal roots, and a mount is
    // never entered.
    if metadata.is_dir() && !walk.is_mount(path, &metadata, parent_dev) {
        for entry in fs::read_dir(path)? {
            preflight_tree(&entry?.path(), depth + 1, metadata.dev(), walk)?;
        }
    }
    Ok(())
}

/// The directories under `data` that something is mounted on, read from a
/// mount table: every mount whose mountpoint is a directory of DATA's own
/// tree, wherever that tree is mounted or bound (a mount on the `/mica`
/// bind's `containers/storage` is DATA's `mica/containers/storage`).
fn data_mounts(data: &Path, mountinfo: &str) -> BTreeSet<PathBuf> {
    struct Mount {
        parent: String,
        device: String,
        root: PathBuf,
        point: PathBuf,
    }
    let unescape = |field: &str| {
        PathBuf::from(
            field
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\"),
        )
    };
    let mut table = BTreeMap::new();
    let mut order = Vec::new();
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [id, parent, device, root, point, ..] = fields[..] else {
            continue;
        };
        order.push(id.to_string());
        table.insert(
            id.to_string(),
            Mount {
                parent: parent.to_string(),
                device: device.to_string(),
                root: unescape(root),
                point: unescape(point),
            },
        );
    }
    // The mount `data` is on: the deepest one holding it, the last of those
    // when several are stacked.
    let mut holder: Option<&Mount> = None;
    for id in &order {
        let mount = &table[id];
        if data.starts_with(&mount.point)
            && holder.is_none_or(|held| {
                mount.point.components().count() >= held.point.components().count()
            })
        {
            holder = Some(mount);
        }
    }
    let Some(holder) = holder else {
        return BTreeSet::new();
    };
    let Ok(below) = data.strip_prefix(&holder.point) else {
        return BTreeSet::new();
    };
    let data_in_fs = holder.root.join(below);
    let mut mounts = BTreeSet::new();
    for mount in table.values() {
        let Some(parent) = table.get(&mount.parent) else {
            continue;
        };
        if parent.device != holder.device {
            continue;
        }
        let Ok(relative) = mount.point.strip_prefix(&parent.point) else {
            continue;
        };
        let dentry = parent.root.join(relative);
        if let Ok(inside) = dentry.strip_prefix(&data_in_fs)
            && !inside.as_os_str().is_empty()
        {
            mounts.insert(data.join(inside));
        }
    }
    mounts
}

fn preflight_reset(roots: &Roots, tier: ResetTier, mounts: &BTreeSet<PathBuf>) -> Result<()> {
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
            .chain([roots.containers()])
            .collect(),
    };
    let mut walk = Walk::new(mounts);
    for target in targets {
        let dev = parent_dev(&target)?;
        preflight_tree(&target, 0, dev, &mut walk)?;
    }
    Ok(())
}

/// Base's tool that builds `/etc/passwd` and `/etc/group` from the image's
/// accounts and those kept on STATE.
const ACCOUNTS_TOOL: &str = "/usr/lib/mica/mica-accounts";

/// Have mica-accounts bind `/etc/passwd` and `/etc/group` again from what
/// STATE holds, after tier 2 cleared the service accounts there. A root
/// without it has nothing to rebuild.
pub fn rebuild_accounts() {
    if !Path::new(ACCOUNTS_TOOL).exists() {
        return;
    }
    match std::process::Command::new(ACCOUNTS_TOOL)
        .arg("build")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "mica-accounts build"
        ),
        Err(err) => tracing::warn!(%err, "mica-accounts build"),
    }
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
    let mountinfo = fs::read_to_string(&roots.mountinfo)
        .with_context(|| format!("read {}", roots.mountinfo.display()))?;
    let mounts = data_mounts(&roots.data, &mountinfo);
    preflight_reset(roots, intent.tier, &mounts)?;
    // dockerd starts after micad, so it is not running yet on a sound boot.
    anyhow::ensure!(
        intent.tier == ResetTier::Configuration || !crate::docker::running(&roots.docker_pid),
        "dockerd is running, and the reset would clear its storage under it"
    );
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
            // operation tier 2 already performs on `volumes/` and `containers/`.
            let config = roots.system().join(CONFIG_DIR);
            clear_contents(&config, &mounts)
                .with_context(|| format!("clear {}", config.display()))?;
        }
        ResetTier::ApplicationData => clear_application_state(roots, &mounts)?,
    }

    if intent.tier == ResetTier::Configuration {
        for file in [APID_IDENTITY, BOARD_DEFAULTS_RECORD] {
            let path = roots.state.join(file);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("remove {}", path.display()));
                }
            }
        }
    }

    // The commit. Built from the tree in hand and saved once; `*settings` is
    // replaced only after the save has returned, so no caller observes a tree
    // that is not on STATE.
    let mut applied = match intent.tier {
        ResetTier::Configuration => reseeded_settings(settings),
        // Tier 2 keeps the operator's configuration and takes their
        // applications. A declared container IS an operator application --
        // the entry is what micad declares to mica-containerd and what brings
        // the workload back -- so clearing the data under `/mica/` without clearing the
        // declarations would leave a reset device running the same workload
        // against an empty volume.
        ResetTier::ApplicationData => Settings {
            container: micad_settings::ContainerSettings {
                projects: std::collections::BTreeMap::new(),
                project_dirs: std::collections::BTreeMap::new(),
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
            // The user's services, whose directories the tier clears.
            services: micad_settings::ServicesSettings::default(),
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

/// The tree the configuration tier leaves behind.
fn reseeded_settings(before: &Settings) -> Settings {
    let mut after = Settings {
        // The identity record, PRESERVED (footnote `[^identity]`), with
        // `state` put back so first-boot provisioning re-seeds the hostname
        // from it and the SSH default.
        provisioning: micad_settings::ProvisioningSettings {
            state: ProvisioningState::Pending,
            ..before.provisioning.clone()
        },
        ..Settings::default()
    };
    // The per-device credential and its generation: a first-boot secret, not a
    // management credential, and `identity::ensure_identity` would re-mint it
    // if it were dropped. the `identity: preserved` covers it.
    after.access.device = before.access.device.clone();
    // footnote `[^cfg]`: tier 1 keeps the apid management credential.
    // A configuration reset that dropped it would be a lockout dressed as
    // a settings action, and a remote credential-clearing primitive —
    // credential recovery is where a credential is deliberately replaced, under the authority and nowhere else.
    after.access.web_admin = before.access.web_admin.clone();
    after.access.claim = before.access.claim;
    after.access.api_tokens = before.access.api_tokens.clone();
    // The access-point key set at the claim: without it a claimed device
    // would fall back to a key nobody was shown
    // (`docs/factory-ap-key.md`).
    after.wifi.ap.psk = before.wifi.ap.psk.clone();
    after
}

/// Tier 2's whole scope: the application layer's
/// enrolment records on STATE, its subtrees under `/mica`, and the container
/// store. `/srv` is the user's and stays, and so does every mount in these
/// trees.
fn clear_application_state(roots: &Roots, mounts: &BTreeSet<PathBuf>) -> Result<()> {
    for dir in STATE_APPLICATION_DIRS {
        let path = roots.state.join(dir);
        clear_contents(&path, mounts).with_context(|| format!("clear {}", path.display()))?;
    }
    for dir in APPLICATION_DIRS {
        let path = roots.system().join(dir);
        clear_contents(&path, mounts).with_context(|| format!("clear {}", path.display()))?;
    }
    // `/srv` is the user's: no tier opens it.
    reseed_tree(&roots.containers(), &["networks", "tmp"], mounts)
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

/// Remove every entry inside `dir`, keeping `dir` itself and every mount.
fn clear_contents(dir: &Path, mounts: &BTreeSet<PathBuf>) -> Result<()> {
    let mut walk = Walk::new(mounts);
    if root_is_mount(dir, &walk)? {
        return Ok(());
    }
    clear_directory(dir, 0, &mut walk)
}

fn clear_directory(dir: &Path, depth: usize, walk: &mut Walk) -> Result<()> {
    walk.traversal.visit(depth)?;
    let dev = match dir.symlink_metadata() {
        Ok(metadata) => metadata.dev(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry of {}", dir.display()))?;
        remove(&entry.path(), &entry.file_type()?, depth + 1, dev, walk)?;
    }
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Reduce `root` to exactly `skeleton`, with every directory in it empty.
///
/// The declared paths are kept and descended into; everything else is removed.
/// Descending rather than deleting the whole tree keeps the modes
/// `mica-data-layout` established, so this module never restates them.
fn reseed_tree(root: &Path, skeleton: &[&str], mounts: &BTreeSet<PathBuf>) -> Result<()> {
    let declared: BTreeSet<PathBuf> = skeleton.iter().map(PathBuf::from).collect();
    let mut walk = Walk::new(mounts);
    if root_is_mount(root, &walk)? {
        return Ok(());
    }
    reseed_dir(root, Path::new(""), &declared, 0, &mut walk)
}

fn reseed_dir(
    dir: &Path,
    relative: &Path,
    declared: &BTreeSet<PathBuf>,
    depth: usize,
    walk: &mut Walk,
) -> Result<()> {
    walk.traversal.visit(depth)?;
    let dev = match dir.symlink_metadata() {
        Ok(metadata) => metadata.dev(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read {}", dir.display())),
    };
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
            let path = entry.path();
            if !walk.is_mount(&path, &path.symlink_metadata()?, dev) {
                reseed_dir(&path, &child, declared, depth + 1, walk)?;
            }
        } else {
            remove(&entry.path(), &file_type, depth + 1, dev, walk)?;
        }
    }
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn remove(
    path: &Path,
    file_type: &fs::FileType,
    depth: usize,
    parent_dev: u64,
    walk: &mut Walk,
) -> Result<()> {
    walk.traversal.visit(depth)?;
    let removed = if file_type.is_dir() {
        if walk.is_mount(path, &path.symlink_metadata()?, parent_dev) {
            return Ok(());
        }
        clear_directory(path, depth, walk)?;
        if walk.holds_mount(path) {
            return Ok(());
        }
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // A directory that is a mountpoint in another mount namespace, or
        // through a bind this one does not see: what was on DATA under it is
        // cleared, and the mount stays.
        Err(err) if err.kind() == std::io::ErrorKind::ResourceBusy => {
            tracing::info!(
                path = %path.display(),
                "a mount elsewhere in a tree the reset clears: left in place"
            );
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests;
