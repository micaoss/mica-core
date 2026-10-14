//! Documents on a claimed device, and the import record.

use micad_settings::ProvisioningState;
use micad_settings::{Settings, Store};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use super::*;

// The claim gate, stated on its own: an unsigned medium cannot reconfigure
// a device that already has an administrator.
#[test]
pub(super) fn a_claimed_device_refuses_a_document_it_has_not_already_applied() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let mut settings = Settings::default();
    settings
        .set(
            "access.webAdmin",
            serde_json::json!({ "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$ZGV2$ZGV2" }),
        )
        .expect("claim the device");
    store.save(&settings).expect("save");
    let claimed = settings.clone();

    let root = stage(dir.path(), Source::Media, time_only_document());
    let outcome = import(&store, &mut settings, &root).expect("import");
    let Outcome::Rejected { rejection, .. } = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert!(
        rejection.reason.contains("already claimed"),
        "{rejection:?}"
    );

    // Nothing the document named moved, and the credential is intact.
    assert_eq!(settings.time, claimed.time);
    assert_eq!(settings.access.web_admin, claimed.access.web_admin);
}

// A device claimed BY the offered document reports `unchanged`, not
// `already-claimed`: the short-circuit runs before the gate, or every
// reboot with the medium still in place would look like an attack.
#[test]
pub(super) fn the_document_that_claimed_the_device_still_reports_unchanged() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Boot, &full_document());
    let mut settings = Settings::default();
    assert!(matches!(
        import(&store, &mut settings, &root).expect("first"),
        Outcome::Applied { .. }
    ));
    assert!(settings.access.web_admin.is_some(), "the device is claimed");

    let mut reloaded = store.load().expect("reload");
    assert!(matches!(
        import(&store, &mut reloaded, &root).expect("second"),
        Outcome::Unchanged { .. }
    ));
}

// An unusable STATE parent fails before any configuration replacement;
// the caller retains its previous tree.
#[test]
pub(super) fn a_failed_save_leaves_state_and_the_caller_untouched() {
    let dir = TempDir::new().expect("tempdir");
    // The settings file's parent is a regular file, so `create_dir_all`
    // inside `Store::save` fails. Root-safe: a type error on the path, not
    // a permission check.
    let blocker = dir.path().join("blocked");
    fs::write(&blocker, b"not a directory").expect("write blocker");
    let config = dir.path().join("config");
    fs::create_dir_all(&config).expect("create the config namespace");
    let store = Store::new(blocker.join("settings.toml"), config);
    let root = stage(dir.path(), Source::Boot, time_only_document());
    let mut settings = Settings::default();

    let err = import(&store, &mut settings, &root).expect_err("the save must fail");
    assert!(
        format!("{err:#}").contains("persist the applied provisioning document"),
        "unexpected error: {err:#}"
    );
    assert_eq!(settings, Settings::default());
    assert_eq!(
        fs::read(&blocker).expect("re-read blocker"),
        b"not a directory"
    );
    assert!(!blocker.join("settings.toml").exists());
}

// The interlock with Layer 1: an identity the factory injected is the
// identity the device keeps, and the seeded hostname is derived from it.
// This is why the import runs BEFORE `ensure_provisioned`.
#[test]
pub(super) fn an_injected_identity_is_the_one_first_boot_seeds_from() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(
        dir.path(),
        Source::Boot,
        "version = 1\n\n[identity]\ndeviceId = \"fedcba9876543210fedcba9876543210\"\n",
    );
    let mut settings = Settings::default();

    import(&store, &mut settings, &root).expect("import");
    crate::provisioning::ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");

    assert_eq!(
        settings.provisioning.device_id.as_deref(),
        Some("fedcba9876543210fedcba9876543210"),
        "seeding must keep the injected identity, not mint over it"
    );
    assert_eq!(settings.hostname, "mica-fedcba98");
    assert_eq!(settings.provisioning.state, ProvisioningState::Complete);
    assert_eq!(settings.provisioning.seeded_generation, 1);
    // The document record survives seeding's own save.
    assert!(settings.provisioning.document.is_some());
}

// A refused document leaves an appliance a person can still set up: no
// credential, no settings change, and `POST /api/v1/setup` still available
// because `access.webAdmin` is absent.
#[test]
pub(super) fn a_refused_document_leaves_a_working_unclaimed_appliance() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Media, "version = 9\n");
    let mut settings = Settings::default();

    assert!(matches!(
        import(&store, &mut settings, &root).expect("import"),
        Outcome::Rejected { .. }
    ));
    // Seeding still runs and still works, which is what "does not block
    // boot" means for the daemon that follows.
    crate::provisioning::ensure_provisioned(&store, dir.path(), &mut settings).expect("seed");
    assert_eq!(settings.provisioning.state, ProvisioningState::Complete);
    assert!(
        settings.access.web_admin.is_none(),
        "the device must still be claimable"
    );
    assert!(settings.access.device.password_hash.is_some());
}

// The record is written once, not once per boot: a device that keeps
// meeting the same refusal does not rewrite STATE every time.
#[test]
pub(super) fn a_repeated_refusal_writes_the_record_once() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Boot, "version = 2\n");
    let mut settings = Settings::default();

    import(&store, &mut settings, &root).expect("first");
    let bytes = fs::read(settings_path(dir.path())).expect("read settings file");

    let mut reloaded = store.load().expect("reload");
    import(&store, &mut reloaded, &root).expect("second");
    assert_eq!(
        fs::read(settings_path(dir.path())).expect("re-read"),
        bytes,
        "the same refusal twice must not rewrite the settings file"
    );
}

// The document version is the DOCUMENT's, and it is pinned here so a bump
// is a decision rather than a side effect.
#[test]
pub(super) fn the_document_version_is_pinned() {
    assert_eq!(DOCUMENT_VERSION, 1);
}

// The two source names are the wire values the status route serves and the
// directory names the transport unit writes into. Pinned so a rename shows
// up here rather than as a unit that stages into a directory nothing reads.
#[test]
pub(super) fn the_source_names_are_the_transport_contract() {
    assert_eq!(SOURCES.len(), 2);
    assert_eq!(Source::Boot.as_str(), "boot");
    assert_eq!(Source::Boot.dir_name(), "boot");
    assert_eq!(Source::Media.as_str(), "media");
    assert_eq!(Source::Media.dir_name(), "media");
    assert_eq!(DOCUMENT_FILE_NAME, "mica-provisioning.toml");
    assert_eq!(DEFAULT_STAGING_ROOT, "/run/mica/provisioning");
}

// The staging root is the default unless the test hook names another.
#[test]
pub(super) fn the_staging_root_defaults_to_the_documented_location() {
    // SAFETY-adjacent: this test reads and does not write the variable, so
    // it cannot race another test's environment.
    if std::env::var_os("MICAD_PROVISIONING_ROOT").is_none() {
        assert_eq!(staging_root_from_env(), PathBuf::from(DEFAULT_STAGING_ROOT));
    }
}
#[test]
pub(super) fn a_failed_import_does_not_modify_configuration_on_disk() {
    let dir = TempDir::new().expect("tempdir");
    let blocker = dir.path().join("blocked");
    fs::write(&blocker, b"not a directory").unwrap();
    let config = dir.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let store = Store::new(blocker.join("settings.toml"), config.clone());
    let root = stage(dir.path(), Source::Boot, time_only_document());
    let mut settings = Settings::default();
    import(&store, &mut settings, &root).expect_err("STATE commit must fail");
    assert_eq!(settings, Settings::default());
    assert!(
        !config.join("time.json").exists(),
        "The import failed but time.json already contains the new provisioning configuration"
    );
}
