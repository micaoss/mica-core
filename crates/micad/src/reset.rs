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

/// The STATE directories holding application enrolment records.
const STATE_APPLICATION_DIRS: &[&str] = &["quadlet", "systemd-units"];

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

    // The commit. Built from the tree in hand and saved once; `*settings` is
    // replaced only after the save has returned, so no caller observes a tree
    // that is not on STATE.
    let mut applied = match intent.tier {
        ResetTier::Configuration | ResetTier::FullFactory => {
            reseeded_settings(settings, intent.tier)
        }
        ResetTier::ApplicationData => settings.clone(),
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
mod tests {
    use micad_settings::{
        ApiToken, ClaimChannel, ClaimSettings, DeviceCredentialSettings, ProvisioningSettings,
        ResetSettings, WebAdminSettings,
    };
    use tempfile::TempDir;

    use super::*;

    /// The sentinel credential. Nothing else in this file can produce it, so
    /// an assertion that it survived is an assertion about this value and not
    /// about a shape that happens to match.
    const ADMIN_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$UkVTRVQ$UkVTRVRIQVNI";
    const DEVICE_ID: &str = "0123456789abcdef0123456789abcdef";

    /// A device as a fielded one is: claimed, named after its identity, with
    /// operator settings in every subtree the schema has.
    fn fielded_settings() -> Settings {
        let mut settings = Settings {
            hostname: "edge-42".to_string(),
            provisioning: ProvisioningSettings {
                state: ProvisioningState::Complete,
                device_id: Some(DEVICE_ID.to_string()),
                seeded_generation: 1,
                document: None,
            },
            ..Settings::default()
        };
        settings.access.web_admin = Some(WebAdminSettings {
            password_hash: ADMIN_HASH.to_string(),
        });
        settings.access.claim = Some(ClaimSettings {
            via: ClaimChannel::Setup,
            at: 1_700_000_000,
            rotation_required: false,
        });
        settings.access.api_tokens = vec![ApiToken {
            id: "3f2a9c41".to_string(),
            name: "ci".to_string(),
            hash: "0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            created: 1_700_000_000,
        }];
        settings.access.device = DeviceCredentialSettings {
            password_hash: Some("$argon2id$v=19$m=19456,t=2,p=1$ZGV2$ZGV2aGFzaA".to_string()),
            generation: 3,
        };
        settings.access.ssh.enabled = true;
        settings.access.ssh.port = 2222;
        settings.container.enabled = true;
        settings.time.timezone = "Europe/Berlin".to_string();
        settings
    }

    /// A pool and a STATE partition populated the way a running device
    /// populates them: an application payload, a verified update bundle, a
    /// custom UI, operator data in `/srv`, and the enrolment records on STATE.
    fn populated_roots() -> (TempDir, Roots) {
        let dir = TempDir::new().unwrap();
        let roots = Roots {
            data: dir.path().join("data"),
            state: dir.path().join("data/state"),
        };
        fs::create_dir_all(roots.data.join("meta")).unwrap();
        write(&roots.data.join("meta/lockdown"), "retained");
        write(
            &roots.state.join("machine-id"),
            "0123456789abcdef0123456789abcdef",
        );
        for relative in SYSTEM_SKELETON {
            fs::create_dir_all(roots.system().join(relative)).unwrap();
        }
        fs::create_dir_all(roots.user()).unwrap();
        fs::create_dir_all(roots.containers().join("networks")).unwrap();
        fs::create_dir_all(roots.containers().join("tmp")).unwrap();
        for dir in STATE_APPLICATION_DIRS {
            fs::create_dir_all(roots.state.join(dir)).unwrap();
        }
        // The settings store and the per-device secrets live on STATE too, and
        // no tier may reach them.
        fs::create_dir_all(roots.state.join("mica/secrets")).unwrap();

        write(&roots.system().join("apps/inventory/db.sqlite"), "app data");
        write(&roots.containers().join("overlay/layer"), "layer");
        write(&roots.system().join("ui/active/index.html"), "custom ui");
        write(
            &roots.system().join("updates/verified/deployment.json"),
            "descriptor",
        );
        write(&roots.system().join("home/operator/.profile"), "profile");
        write(&roots.system().join("root/.ssh/known_hosts"), "hosts");
        write(&roots.user().join("operator/report.csv"), "operator data");
        write(&roots.state.join("quadlet/web.container"), "unit");
        write(&roots.state.join("systemd-units/vendor.service"), "unit");
        write(&roots.state.join("mica/secrets/device-password"), "secret");
        (dir, roots)
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn exists(path: &Path) -> bool {
        path.symlink_metadata().is_ok()
    }

    /// Every path a tier may NOT have opened, with what it must still hold.
    ///
    /// Called by all three tier tests: META and the slots are outside these
    /// roots entirely, and these are the stores inside them that survive every
    /// tier.
    fn assert_identity_survived(roots: &Roots) {
        assert_eq!(
            fs::read_to_string(roots.state.join("machine-id")).unwrap(),
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            fs::read_to_string(roots.data.join("meta/lockdown")).unwrap(),
            "retained"
        );
        assert_eq!(
            fs::read_to_string(roots.state.join("mica/secrets/device-password")).unwrap(),
            "secret",
            "a tier reached the per-device secrets"
        );
    }

    fn store_at(dir: &TempDir, roots: &Roots) -> Store {
        Store::new(
            dir.path().join("settings.toml"),
            roots.system().join(CONFIG_DIR),
        )
    }

    fn stage(settings: &mut Settings, tier: ResetTier) {
        settings.reset = Some(ResetSettings {
            tier,
            requested: 1_700_000_000,
            presence: (tier == ResetTier::FullFactory).then(|| "console-attach".to_string()),
        });
    }

    /// No record, no reset: a boot with nothing staged writes nothing at all.
    ///
    /// The positive control for every test below — without it, an applier that
    /// never ran would pass the survival assertions vacuously.
    #[test]
    fn a_boot_with_no_intent_staged_changes_nothing() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        let before = settings.clone();

        assert_eq!(
            apply_pending(&store, &mut settings, &roots).unwrap(),
            Outcome::NoIntent
        );

        assert_eq!(settings, before);
        assert!(!dir.path().join("settings.toml").exists(), "it saved");
        assert!(exists(&roots.system().join("apps/inventory/db.sqlite")));
        assert!(exists(&roots.user().join("operator/report.csv")));
    }

    /// Tier 1, row 1: STATE re-seeded, everything else in reach
    /// preserved, and the management credential above all.
    #[test]
    fn tier_one_reseeds_the_settings_and_keeps_the_management_credential() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::Configuration);

        assert_eq!(
            apply_pending(&store, &mut settings, &roots).unwrap(),
            Outcome::Applied(ResetTier::Configuration)
        );

        // CLEARED: every modelled setting, at once. There is no per-subtree
        // reset, so the assertion is over subtrees the operator touched.
        assert_eq!(settings.hostname, Settings::default().hostname);
        assert_eq!(settings.access.ssh, Default::default());
        assert_eq!(settings.container, Default::default());
        assert_eq!(settings.time, Default::default());

        // PRESERVED, and this is the row that a reset clearing more than its
        // scope would break: the credential, the claim record, the tokens, the
        // per-device secret and the identity.
        assert_eq!(
            settings.access.web_admin.as_ref().unwrap().password_hash,
            ADMIN_HASH
        );
        assert_eq!(settings.access.claim.unwrap().via, ClaimChannel::Setup);
        assert_eq!(settings.access.api_tokens.len(), 1);
        assert_eq!(settings.access.device.generation, 3);
        assert_eq!(settings.provisioning.device_id.as_deref(), Some(DEVICE_ID));
        // Handed back to first-boot provisioning, which re-derives the
        // hostname from the identity above.
        assert_eq!(settings.provisioning.state, ProvisioningState::Pending);

        // UNAFFECTED: tier 1 opens no DATA store and no STATE directory but
        // the settings file.
        assert!(exists(&roots.system().join("apps/inventory/db.sqlite")));
        assert!(exists(&roots.containers().join("overlay/layer")));
        assert!(exists(&roots.user().join("operator/report.csv")));
        assert!(exists(&roots.state.join("quadlet/web.container")));
        assert_identity_survived(&roots);

        // The record is gone, and the tree on STATE is the tree in hand.
        assert_eq!(settings.reset, None);
        assert_eq!(store.load().unwrap(), settings);
    }

    /// Tier 2, row 2: the application layer goes and nothing else does —
    /// not the platform's settings, not its credentials, not `/mica/ui`, not a
    /// verified bundle.
    #[test]
    fn tier_two_clears_the_application_layer_and_keeps_the_platform() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        let before_settings = Settings {
            reset: None,
            ..settings.clone()
        };
        stage(&mut settings, ResetTier::ApplicationData);

        assert_eq!(
            apply_pending(&store, &mut settings, &roots).unwrap(),
            Outcome::Applied(ResetTier::ApplicationData)
        );

        // CLEARED: the application subtrees of `/mica`, all of `/srv`, and the
        // enrolment records on STATE.
        assert!(!exists(&roots.system().join("apps/inventory")));
        assert!(!exists(&roots.containers().join("overlay")));
        assert!(!exists(&roots.user().join("operator")));
        assert!(!exists(&roots.state.join("quadlet/web.container")));
        assert!(!exists(&roots.state.join("systemd-units/vendor.service")));

        // RE-SEEDED and not deleted: the directories are still there at the
        // modes `mica-data-layout` gave them, so nothing that writes into them
        // has to wait for the next boot.
        for relative in APPLICATION_DIRS {
            assert!(roots.system().join(relative).is_dir(), "{relative}");
        }
        assert!(roots.user().is_dir(), "the /srv mount point was removed");

        // PRESERVED: `/mica` is the system-owned namespace and tier 2 does not
        // empty it — a verified bundle is not application data.
        assert_eq!(
            fs::read_to_string(roots.system().join("updates/verified/deployment.json")).unwrap(),
            "descriptor"
        );
        assert!(exists(&roots.system().join("ui/active/index.html")));
        assert!(exists(&roots.system().join("home/operator/.profile")));
        assert!(exists(&roots.system().join("root/.ssh/known_hosts")));
        assert_identity_survived(&roots);

        // PRESERVED: the whole settings tree, credential included. The only
        // write is the record's removal.
        assert_eq!(settings, before_settings);
        assert_eq!(store.load().unwrap(), before_settings);
    }

    #[test]
    fn application_and_factory_reset_clear_the_independent_container_namespace() {
        for tier in [ResetTier::ApplicationData, ResetTier::FullFactory] {
            let (dir, roots) = populated_roots();
            let store = store_at(&dir, &roots);
            let mut settings = fielded_settings();
            let containers = roots.data.join("containers");
            write(
                &containers.join("storage/overlay/layer"),
                "container payload",
            );
            fs::create_dir_all(containers.join("networks")).unwrap();
            stage(&mut settings, tier);
            apply_pending(&store, &mut settings, &roots).unwrap();
            assert!(!containers.join("storage").exists());
            assert!(containers.join("networks").is_dir());
            assert!(containers.join("tmp").is_dir());
        }
    }

    /// Tier 3, row 3: the whole mutable state goes; identity,
    /// calibration and the per-device secrets do not.
    #[test]
    fn tier_three_returns_the_device_to_first_boot_and_keeps_its_identity() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::FullFactory);

        assert_eq!(
            apply_pending(&store, &mut settings, &roots).unwrap(),
            Outcome::Applied(ResetTier::FullFactory)
        );

        // CLEARED: settings, management credentials, applications and all
        // operator data, together.
        assert_eq!(settings.hostname, Settings::default().hostname);
        assert_eq!(settings.access.web_admin, None);
        assert_eq!(settings.access.claim, None);
        assert!(settings.access.api_tokens.is_empty());
        assert_eq!(settings.access.ssh, Default::default());
        assert!(!exists(&roots.system().join("apps/inventory")));
        assert!(!exists(&roots.system().join("ui/active")));
        assert!(!exists(
            &roots.system().join("updates/verified/deployment.json")
        ));
        assert!(!exists(&roots.system().join("home/operator")));
        assert!(!exists(&roots.user().join("operator")));
        assert!(!exists(&roots.state.join("quadlet/web.container")));

        // RE-SEEDED: `/mica` is exactly the skeleton a virgin device has, every
        // directory present and every one of them empty.
        for relative in SYSTEM_SKELETON {
            let path = roots.system().join(relative);
            assert!(path.is_dir(), "{relative} is not a directory");
        }
        assert_eq!(
            fs::read_dir(roots.system().join("apps")).unwrap().count(),
            0
        );
        assert_eq!(
            fs::read_dir(roots.system().join("updates/verified"))
                .unwrap()
                .count(),
            0
        );

        // PRESERVED: identity, the per-device credential, and the secrets on
        // STATE. A full factory reset does not re-mint identity — the
        // operation that does is the whole-disk reflash.
        assert_eq!(settings.provisioning.device_id.as_deref(), Some(DEVICE_ID));
        assert_eq!(settings.access.device.generation, 3);
        assert_eq!(
            settings.access.device.password_hash,
            fielded_settings().access.device.password_hash
        );
        assert_identity_survived(&roots);
        assert_eq!(settings.provisioning.state, ProvisioningState::Pending);

        assert_eq!(settings.reset, None);
        assert_eq!(store.load().unwrap(), settings);
    }

    /// The tier that has to survive an interruption, driven the way the record
    /// makes it survivable: the filesystem work is done and the commit has not
    /// landed, so the record is still staged and a replay finishes the job.
    #[test]
    fn an_interrupted_tier_replays_to_the_same_device() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);

        // The interruption, modelled where it is observable: the tier's
        // filesystem work ran, the save did not, so the record is untouched on
        // the tree the next boot loads.
        let mut interrupted = fielded_settings();
        stage(&mut interrupted, ResetTier::FullFactory);
        clear_application_state(&roots).unwrap();
        reseed_tree(&roots.system(), SYSTEM_SKELETON).unwrap();
        assert!(
            interrupted.reset.is_some(),
            "the record must survive the interruption"
        );

        // The replay, over a tree half of the work has already been done to.
        assert_eq!(
            apply_pending(&store, &mut interrupted, &roots).unwrap(),
            Outcome::Applied(ResetTier::FullFactory)
        );

        // And it is the device the uninterrupted path produces.
        let (other_dir, other_roots) = populated_roots();
        let other_store = store_at(&other_dir, &other_roots);
        let mut uninterrupted = fielded_settings();
        stage(&mut uninterrupted, ResetTier::FullFactory);
        apply_pending(&other_store, &mut uninterrupted, &other_roots).unwrap();
        assert_eq!(interrupted, uninterrupted);
    }

    /// Running the same tier twice over its own output changes nothing the
    /// second time. Idempotence is what makes "re-run the tier" a safe
    /// instruction after a power loss.
    #[test]
    fn a_tier_applied_over_its_own_output_is_a_no_op() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::ApplicationData);
        apply_pending(&store, &mut settings, &roots).unwrap();
        let once = settings.clone();

        stage(&mut settings, ResetTier::ApplicationData);
        apply_pending(&store, &mut settings, &roots).unwrap();

        assert_eq!(settings, once);
    }

    /// A symlink planted in a cleared tree is unlinked, never followed: the
    /// target survives, so a tier cannot be steered into a store that is
    /// `unaffected`.
    #[test]
    fn reset_refuses_excessive_depth_before_removing_any_payload() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut deep = roots.user();
        for _ in 0..65 {
            deep = deep.join("nested");
        }
        fs::create_dir_all(&deep).unwrap();
        write(&deep.join("retained"), "deep payload");
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::FullFactory);
        let error = apply_pending(&store, &mut settings, &roots).unwrap_err();
        assert!(format!("{error:#}").contains("depth"));
        assert!(roots.system().join("apps/inventory/db.sqlite").is_file());
        assert!(deep.join("retained").is_file());
        assert!(settings.reset.is_some());
    }

    #[test]
    fn a_symlink_in_a_cleared_tree_is_unlinked_rather_than_followed() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("meta"), "not this tier's").unwrap();
        std::os::unix::fs::symlink(&outside, roots.user().join("escape")).unwrap();

        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::ApplicationData);
        apply_pending(&store, &mut settings, &roots).unwrap();

        assert!(!exists(&roots.user().join("escape")));
        assert_eq!(
            fs::read_to_string(outside.join("meta")).unwrap(),
            "not this tier's",
            "the tier followed a symlink out of its own roots"
        );
    }

    /// A tier that cannot complete its own scope fails and leaves the record
    /// staged; it does not widen to the next tier and it does not re-seed.
    #[test]
    fn a_tier_that_cannot_finish_leaves_the_intent_staged() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::ApplicationData);
        let before = settings.clone();
        fs::remove_dir_all(roots.user()).unwrap();
        fs::write(roots.user(), "not a directory").unwrap();

        let err = apply_pending(&store, &mut settings, &roots).unwrap_err();

        assert!(format!("{err:#}").contains("srv"), "{err:#}");
        assert_eq!(settings, before, "a failed tier changed the tree");
        assert!(settings.reset.is_some(), "the retry has nothing to replay");
        assert!(
            !dir.path().join("settings.toml").exists(),
            "a failed tier committed"
        );
        // And it did not widen: the STATE and `/mica` work its own row calls
        // for ran, but nothing outside that row was touched.
        assert!(exists(
            &roots.system().join("updates/verified/deployment.json")
        ));
        assert_identity_survived(&roots);
    }

    #[test]
    fn reset_refuses_a_namespace_symlink_before_removing_any_payload() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        write(&outside.join("retained"), "identity");
        fs::remove_dir_all(roots.state.join("quadlet")).unwrap();
        std::os::unix::fs::symlink(&outside, roots.state.join("quadlet")).unwrap();
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::ApplicationData);
        assert!(apply_pending(&store, &mut settings, &roots).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("retained")).unwrap(),
            "identity"
        );
        assert!(roots.system().join("apps/inventory/db.sqlite").exists());
        assert!(settings.reset.is_some());
    }

    #[test]
    fn reset_and_installer_share_one_data_transaction_lock() {
        let (dir, roots) = populated_roots();
        let store = store_at(&dir, &roots);
        fs::create_dir_all(roots.data.join("meta")).unwrap();
        let lock = fs::File::create(roots.data.join("meta/transaction.lock")).unwrap();
        lock.try_lock().unwrap();
        let mut settings = fielded_settings();
        stage(&mut settings, ResetTier::FullFactory);
        assert!(apply_pending(&store, &mut settings, &roots).is_err());
        assert!(roots.system().join("apps/inventory/db.sqlite").exists());
        drop(lock);
        apply_pending(&store, &mut settings, &roots).unwrap();
        assert!(roots.data.join("meta/transaction.lock").exists());
    }

    #[test]
    fn reset_rejects_nested_binds_even_on_the_same_device() {
        let base = "30 1 8:3 / /mnt/data rw - ext4 /dev/vda3 rw\n";
        reject_nested_mounts(Path::new("/mnt/data"), base).unwrap();
        let bound = format!("{base}31 30 8:3 /state /mnt/data/srv/escape rw - ext4 /dev/vda3 rw\n");
        assert!(reject_nested_mounts(Path::new("/mnt/data"), &bound).is_err());
    }
}
