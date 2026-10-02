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
    write(&roots.state.join("containerd/web.json"), "declaration");
    write(&roots.state.join("systemd-units/vendor.service"), "unit");
    write(&roots.state.join("mica/secrets/device-password"), "secret");
    write(&roots.state.join(APID_IDENTITY), "uploaded identity");
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
    assert!(
        !exists(&roots.state.join(APID_IDENTITY)),
        "the console's TLS identity is operator configuration"
    );

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
    assert!(!exists(&roots.state.join("containerd/web.json")));
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
    assert!(exists(&roots.state.join(APID_IDENTITY)));

    // PRESERVED: the whole settings tree, credential included. The only
    // write is the record's removal.
    assert_eq!(settings, before_settings);
    assert_eq!(store.load().unwrap(), before_settings);
}

/// A declared container is an operator application: the tier that takes
/// their applications takes the declarations too, or the reconciler
/// renders them again and systemd starts a workload a reset was supposed
/// to remove. A paired device is the same argument.
#[test]
fn the_application_tier_clears_the_bluetooth_trust_list() {
    let (dir, roots) = populated_roots();
    let store = store_at(&dir, &roots);
    let mut settings = fielded_settings();
    settings.bluetooth.enabled = true;
    settings.bluetooth.devices.insert(
        "AA:BB:CC:DD:EE:01".to_string(),
        micad_settings::PairedDevice {
            name: "phone".to_string(),
            trusted: true,
            blocked: false,
        },
    );
    stage(&mut settings, ResetTier::ApplicationData);

    apply_pending(&store, &mut settings, &roots).unwrap();

    assert!(settings.bluetooth.devices.is_empty());
    // The adapter switch is configuration, and configuration is tier 1's.
    assert!(settings.bluetooth.enabled);
}

/// A declared container is an operator application: the tier that takes
/// their applications takes the declarations too, or the reconciler
/// renders them again and systemd starts a workload a reset was supposed
/// to remove.
#[test]
fn the_application_tier_clears_declared_containers_with_their_data() {
    let (dir, roots) = populated_roots();
    let store = store_at(&dir, &roots);
    let mut settings = fielded_settings();
    settings.container.enabled = true;
    settings.container.units.insert(
        "node-red".to_string(),
        micad_settings::ContainerUnit {
            image: "docker.io/nodered/node-red:4.0.9".to_string(),
            command: Vec::new(),
            environment: std::collections::BTreeMap::new(),
            publish: Vec::new(),
            volumes: Vec::new(),
            restart: micad_settings::RestartPolicy::default(),
            auto_start: true,
            pids: None,
            memory: None,
            cpu: None,
        },
    );
    let hostname = settings.hostname.clone();
    stage(&mut settings, ResetTier::ApplicationData);

    assert_eq!(
        apply_pending(&store, &mut settings, &roots).unwrap(),
        Outcome::Applied(ResetTier::ApplicationData)
    );

    assert!(settings.container.units.is_empty());
    // The switch and the rest of the configuration are tier 1's business,
    // not this tier's: they survive.
    assert!(settings.container.enabled);
    assert_eq!(settings.hostname, hostname);
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
    assert!(
        !exists(&roots.state.join(APID_IDENTITY)),
        "the console's TLS identity is operator configuration"
    );
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
