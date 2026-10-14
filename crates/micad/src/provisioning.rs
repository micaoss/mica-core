//! First-boot self-provisioning: turning an empty STATE into a working device.

use std::path::Path;

use anyhow::{Context, Result};
use micad_settings::{ProvisioningState, Settings, Store};

use crate::identity;

/// Revision of the seeding rules implemented by this module.
///
/// Recorded in `provisioning.seededGeneration` so a later image can tell which
/// rules produced a fielded device's tree. Bump it when the seeded values
/// change in a way a re-seed would have to repair; do not bump it for changes
/// that only affect devices which have not provisioned yet.
pub const SEEDING_GENERATION: u32 = 1;

/// Prefix of a seeded hostname.
const HOSTNAME_PREFIX: &str = "mica-";

/// How many leading hex characters of the device identifier the seeded hostname
/// carries. Eight hex characters is 32 bits, enough that two devices on one
/// LAN colliding is not a practical concern, and short enough to read off a
/// label.
const HOSTNAME_ID_CHARS: usize = 8;

/// What [`ensure_provisioned`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The tree was seeded and persisted.
    Seeded,
    /// `provisioning.state` was already `complete`; nothing was read or written.
    AlreadyProvisioned,
}

/// Provision this device if STATE says it has not been provisioned yet.
pub fn ensure_provisioned(
    store: &Store,
    state_dir: &Path,
    settings: &mut Settings,
) -> Result<Outcome> {
    if settings.provisioning.state == ProvisioningState::Complete {
        return Ok(Outcome::AlreadyProvisioned);
    }

    // Seed into a copy. `*settings` is only replaced once the save that commits
    // the tree has returned, so no caller ever observes a half-seeded tree and
    // no half-seeded tree reaches STATE.
    let mut seeded = settings.clone();

    identity::ensure_identity(state_dir, &mut seeded).context("establish device identity")?;
    let device_id = seeded
        .provisioning
        .device_id
        .clone()
        .context("device identity absent after generation")?;

    if seeded.hostname == Settings::default().hostname {
        seeded.hostname = seeded_hostname(&device_id);
    }

    // SSH is off on a fresh device, whatever image it booted; the operator
    // opens it.
    seeded.access.ssh.enabled = false;

    seeded.provisioning.state = ProvisioningState::Complete;
    seeded.provisioning.seeded_generation = SEEDING_GENERATION;

    store.save(&seeded).context("persist the seeded settings")?;

    tracing::info!(
        hostname = seeded.hostname,
        ssh_enabled = seeded.access.ssh.enabled,
        seeded_generation = seeded.provisioning.seeded_generation,
        "first-boot provisioning complete"
    );
    *settings = seeded;
    Ok(Outcome::Seeded)
}

/// Hostname for a device with this identifier.
///
/// Derived from the device identity and nothing else: no DHCP option, no
/// reverse DNS lookup, no MAC address. That is what lets a device with no
/// network reach a named, working state, and it makes the hostname stable
/// across cables, subnets and NIC replacements.
fn seeded_hostname(device_id: &str) -> String {
    let suffix: String = device_id.chars().take(HOSTNAME_ID_CHARS).collect();
    format!("{HOSTNAME_PREFIX}{suffix}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use micad_settings::{ApMode, ProvisioningSettings, SshSettings, WifiSettings};
    use tempfile::TempDir;

    use super::*;

    fn settings_path(dir: &Path) -> PathBuf {
        dir.join("settings.toml")
    }

    fn store_in(dir: &Path) -> Store {
        // The `/mica/config/` namespace has to exist: an absent document is a
        // default, an absent namespace is the DATA medium being gone, and the
        // store refuses that rather than defaulting.
        let config = dir.join("config");
        fs::create_dir_all(&config).expect("create the config namespace");
        Store::new(settings_path(dir), config)
    }

    /// Every file under `<state_dir>/secrets`, by name, as raw bytes.
    fn secrets_snapshot(state_dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let dir = state_dir.join("secrets");
        let mut out = BTreeMap::new();
        for entry in fs::read_dir(&dir).expect("read secrets dir") {
            let entry = entry.expect("dir entry");
            let bytes = fs::read(entry.path()).expect("read secret");
            out.insert(entry.file_name().to_string_lossy().into_owned(), bytes);
        }
        out
    }

    // A fresh STATE is seeded into a tree the device can actually run on, and
    // that tree is on disk when the call returns.
    #[test]
    fn fresh_state_is_seeded_and_persisted() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let mut settings = Settings::default();

        let outcome =
            ensure_provisioned(&store, dir.path(), &mut settings).expect("ensure_provisioned");
        assert_eq!(outcome, Outcome::Seeded);

        let device_id = settings
            .provisioning
            .device_id
            .as_deref()
            .expect("device_id seeded");
        assert_eq!(device_id.len(), 32, "device_id must be 16 bytes of hex");
        assert_eq!(settings.provisioning.state, ProvisioningState::Complete);
        assert_eq!(settings.provisioning.seeded_generation, 1);
        assert_eq!(settings.hostname, format!("mica-{}", &device_id[..8]));
        assert!(
            !settings.access.ssh.enabled,
            "a fresh device must not open SSH"
        );
        assert!(
            settings.access.device.password_hash.is_some(),
            "the device credential must exist after first boot"
        );

        // Nothing is invented for hardware this build cannot see.
        assert!(
            settings.network.is_empty(),
            "network must stay empty: the image's static 80-dhcp.network already \
             covers eth*, and a seeded entry would render a unit for an \
             interface that may not exist, got {:?}",
            settings.network
        );
        assert_eq!(settings.wifi, WifiSettings::default());
        assert!(!settings.wifi.client.enabled);
        assert_eq!(settings.wifi.ap.mode, ApMode::Off);

        // The one write really happened, and it holds the same tree.
        let on_disk = store.load().expect("load seeded settings");
        assert_eq!(on_disk, settings);
    }

    // The "run twice, same state" acceptance criterion. Note the outcome
    // assertion: without it this test passes even with the `Complete` guard
    // removed, because re-seeding an already seeded tree happens to reproduce
    // the same bytes.
    #[test]
    fn running_twice_is_a_genuine_no_op() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let mut settings = Settings::default();

        assert_eq!(
            ensure_provisioned(&store, dir.path(), &mut settings).expect("first"),
            Outcome::Seeded
        );
        let first_tree = settings.clone();
        let first_bytes = fs::read(settings_path(dir.path())).expect("read settings file");
        let first_secrets = secrets_snapshot(dir.path());
        assert_eq!(first_secrets.len(), 2, "both secrets must exist");

        // Reload from STATE, exactly as the next boot would.
        let mut second = store.load().expect("reload");
        assert_eq!(second, first_tree);
        assert_eq!(
            ensure_provisioned(&store, dir.path(), &mut second).expect("second"),
            Outcome::AlreadyProvisioned
        );

        assert_eq!(second, first_tree, "the whole settings tree must not move");
        assert_eq!(second.provisioning.seeded_generation, 1);
        assert_eq!(
            fs::read(settings_path(dir.path())).expect("re-read settings file"),
            first_bytes,
            "settings.toml must be byte-identical after a second run"
        );
        assert_eq!(
            secrets_snapshot(dir.path()),
            first_secrets,
            "no credential may be regenerated on a re-run"
        );
    }

    // An operator's own values survive. This is the case that catches a guard
    // removal for real: the stored SSH state is the opposite of what
    // seeding writes, and the hostname is not the built-in default.
    #[test]
    fn operator_changes_survive_a_re_run() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());

        let mut settings = Settings::default();
        ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");

        // The operator names the device and opens SSH, as they are entitled to.
        settings.hostname = "edge-42".to_string();
        settings.access.ssh.enabled = true;
        settings.access.ssh.port = 2222;
        store.save(&settings).expect("save operator changes");

        let mut reloaded = store.load().expect("reload");
        assert_eq!(
            ensure_provisioned(&store, dir.path(), &mut reloaded).expect("re-run"),
            Outcome::AlreadyProvisioned
        );

        assert_eq!(reloaded.hostname, "edge-42");
        assert!(
            reloaded.access.ssh.enabled,
            "a re-run must not close SSH the operator opened"
        );
        assert_eq!(reloaded.access.ssh.port, 2222);
        assert_eq!(reloaded, settings);
        assert_eq!(store.load().expect("reload again"), settings);
    }

    // The hostname rule also protects an operator who renamed the device before
    // provisioning finished, e.g. through a pre-seeded STATE.
    #[test]
    fn seeding_does_not_overwrite_a_non_default_hostname() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());

        let mut settings = Settings {
            hostname: "edge-42".to_string(),
            ..Settings::default()
        };
        assert_eq!(settings.provisioning.state, ProvisioningState::Pending);

        ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");
        assert_eq!(settings.hostname, "edge-42");
        assert_eq!(settings.provisioning.state, ProvisioningState::Complete);
    }

    // SSH is seeded off, and nothing else about SSH moves from the schema
    // defaults: persistent access is a public key in
    // `access.ssh.authorizedKeys`, and the transient root password is set at
    // runtime.
    #[test]
    fn a_fresh_device_seeds_ssh_off() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let mut settings = Settings::default();

        ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");
        assert!(!settings.access.ssh.enabled);
        assert!(!store.load().expect("reload").access.ssh.enabled);
        assert_eq!(
            settings.access.ssh,
            SshSettings {
                enabled: false,
                ..SshSettings::default()
            }
        );
    }

    // The offline property. The hostname is a pure function of the device
    // identity, so it needs no DHCP, no DNS and no MAC lookup. This proves the
    // derivation; it does not prove the process makes no syscall, which is
    // covered instead by the module carrying no networking API at all.
    #[test]
    fn hostname_is_derived_only_from_the_device_identity() {
        let seed = |device_id: &str| {
            let dir = TempDir::new().expect("tempdir");
            let store = store_in(dir.path());
            let mut settings = Settings {
                provisioning: ProvisioningSettings {
                    device_id: Some(device_id.to_string()),
                    ..ProvisioningSettings::default()
                },
                ..Settings::default()
            };
            ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");
            // The pre-set identity is kept, not replaced.
            assert_eq!(settings.provisioning.device_id.as_deref(), Some(device_id));
            settings.hostname
        };

        let a = "0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210";
        // Same identity, two independent devices, two independent runs.
        assert_eq!(seed(a), seed(a));
        assert_eq!(seed(a), "mica-01234567");
        // A different identity must produce a different name, or two devices on
        // one LAN would collide.
        assert_ne!(seed(a), seed(b));
        assert_eq!(seed(b), "mica-fedcba98");

        // The two identities differ only after the eighth character, so this
        // catches a hostname built from a fixed prefix rather than the identity.
        let c = "0123456789abcdefffffffffffffffff";
        assert_eq!(seed(a), seed(c));

        // Shorter than the window: a short name, not a panic.
        assert_eq!(seed("abc"), "mica-abc");
    }

    // A failing save must not leave a tree claiming `complete`.
    #[test]
    fn a_failed_save_leaves_no_complete_tree_on_disk() {
        let dir = TempDir::new().expect("tempdir");

        // The settings file's parent directory is a regular file, so
        // `Store::save`'s `create_dir_all` fails. Root-safe: this is a type
        // error on the path, not a permission check.
        let blocker = dir.path().join("blocked");
        fs::write(&blocker, b"not a directory").expect("write blocker");
        let config = dir.path().join("config");
        fs::create_dir_all(&config).expect("create the config namespace");
        let store = Store::new(blocker.join("settings.toml"), config);

        let mut settings = Settings::default();
        let err =
            ensure_provisioned(&store, dir.path(), &mut settings).expect_err("save must fail");
        assert!(
            format!("{err:#}").contains("persist the seeded settings"),
            "unexpected error: {err:#}"
        );

        // Nothing claiming `complete` reached STATE, and the caller's tree was
        // not advanced either — the next boot retries from scratch.
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.provisioning.state, ProvisioningState::Pending);
        assert_eq!(
            fs::read(&blocker).expect("re-read blocker"),
            b"not a directory",
            "the failed save must not have written anything"
        );
        assert!(!blocker.join("settings.toml").exists());
        assert!(!dir.path().join("settings.toml").exists());
    }

    // And a failure *before* the save must leave an existing on-disk tree
    // byte-identical, not partially rewritten.
    #[test]
    fn a_failed_identity_step_leaves_the_on_disk_tree_untouched() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        store.save(&Settings::default()).expect("seed the file");
        let before = fs::read(settings_path(dir.path())).expect("read settings file");

        // The state directory is a regular file, so the secrets directory
        // cannot be created and `ensure_identity` fails.
        let state_dir = dir.path().join("state-is-a-file");
        fs::write(&state_dir, b"x").expect("write blocker");

        let mut settings = store.load().expect("load");
        let err =
            ensure_provisioned(&store, &state_dir, &mut settings).expect_err("identity must fail");
        assert!(
            format!("{err:#}").contains("establish device identity"),
            "unexpected error: {err:#}"
        );

        assert_eq!(settings.provisioning.state, ProvisioningState::Pending);
        assert!(settings.provisioning.device_id.is_none());
        assert_eq!(
            fs::read(settings_path(dir.path())).expect("re-read settings file"),
            before,
            "settings.toml must be byte-identical after a failed provisioning run"
        );
        assert_eq!(
            store.load().expect("reload").provisioning.state,
            ProvisioningState::Pending
        );
    }

    // The seeding revision is recorded, and it is the module's constant rather
    // than a counter that moves on every boot.
    #[test]
    fn seeded_generation_records_the_seeding_revision() {
        assert_eq!(SEEDING_GENERATION, 1);

        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let mut settings = Settings::default();
        assert_eq!(settings.provisioning.seeded_generation, 0);

        ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");
        assert_eq!(settings.provisioning.seeded_generation, 1);
        assert_eq!(
            store.load().expect("reload").provisioning.seeded_generation,
            1
        );

        // The field records *which* rules seeded the tree, not how many
        // attempts it took. A tree left `pending` by an interrupted run that
        // already carried a generation therefore gets stamped, not incremented.
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let mut interrupted = Settings {
            provisioning: ProvisioningSettings {
                seeded_generation: 5,
                ..ProvisioningSettings::default()
            },
            ..Settings::default()
        };
        ensure_provisioned(&store, dir.path(), &mut interrupted).expect("re-seed");
        assert_eq!(interrupted.provisioning.seeded_generation, 1);
    }
}
