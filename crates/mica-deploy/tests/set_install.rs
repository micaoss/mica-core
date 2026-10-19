//! A core set from installation to the end of its trial: published beside the
//! current one (every device has one: its image's seed set), tried by the runkit while its trial has boots left, then
//! committed by a healthy boot or dropped.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/ubi.rs"]
mod ubi;

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    components::component_id,
    core_set::{self, BootRoot, CoreBoot},
    deployments::{
        BootBackend, CoreChoice, DeploymentStore, SetSettlement, SetTarget, State, Target,
    },
    set_state::{self, SetState},
    update_set::SetKind,
};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;

fn sha(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}
fn signer() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[11; 32]).unwrap()
}
fn key() -> [u8; 32] {
    signer().public_key().as_ref().try_into().unwrap()
}

/// A component's image: three 4096-byte blocks, two of data and one of tree,
/// which is the smallest image the verity geometry accepts.
fn image(fill: u8) -> Vec<u8> {
    vec![fill; 12288]
}
fn signature(fill: u8) -> Vec<u8> {
    vec![fill; 64]
}

fn component(
    package: &str,
    version: &str,
    feature: &str,
    needs: Value,
    fill: u8,
    requires: &Value,
) -> Value {
    let mut core = json!({
        "schema": "mica/core/v1", "arch": "amd64", "package": package, "version": version,
        "features": [feature], "needs": needs, "root": {"min": 1},
        "content": {
            "image": {"bytes": 12288, "sha256": sha(&image(fill))},
            "rootHash": "ab".repeat(32),
            "signature": {"bytes": 64, "sha256": sha(&signature(fill))},
            "verity": {"version": 1, "algorithm": "sha256", "dataBlockSize": 4096,
                "hashBlockSize": 4096, "dataBlocks": 2, "hashOffset": 8192, "salt": "cd".repeat(32)},
        },
    });
    if !requires.is_null() {
        core["requires"] = requires.clone();
    }
    core["id"] = json!(component_id(&core).unwrap());
    core
}

/// A signed set of `micad` (image `fill`) and a console that needs it (image
/// 200), with its id.
fn core_set(channel: &str, generation: u64, version: &str, fill: u8) -> (Vec<u8>, String) {
    requiring(channel, generation, version, fill, &Value::Null)
}

/// As [`core_set`], with `micad` requiring the root packages `requires`.
fn requiring(
    channel: &str,
    generation: u64,
    version: &str,
    fill: u8,
    requires: &Value,
) -> (Vec<u8>, String) {
    let payload = serde_json::to_vec(&json!({
        "schema": "mica/set/v1", "kind": "core", "channel": channel, "arch": "amd64",
        "generation": generation, "version": version,
        "components": [
            component("mica-apid-ui", version, "ui",
                json!([{"package": "micad", "min": version, "max": version}]), 200, &Value::Null),
            component("micad", version, "micad", json!([]), fill, requires),
        ],
    }))
    .unwrap();
    let signer = signer();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha(signer.public_key().as_ref()),
        STANDARD.encode(&payload),
        STANDARD.encode(signer.sign(&payload).as_ref())
    )
    .into_bytes();
    (envelope, sha(&payload))
}

struct Device {
    _dir: TempDir,
    store: DeploymentStore,
    objects: std::path::PathBuf,
    features: Vec<String>,
    /// The UBI device of a `ubi` board.
    ubi: Option<std::sync::Arc<ubi::DirUbi>>,
}

/// The seed set an image carries: general generation 1, micad's image 1.
fn seed() -> (Vec<u8>, String) {
    core_set("general", 1, "0.0.5", 1)
}

impl Device {
    /// A device with one confirmed deployment, no core set yet, and every
    /// object a test's sets name already downloaded.
    fn bare() -> Self {
        Self::bare_on(false)
    }

    /// As [`Self::bare`], on a UEFI board or on a `ubi` one.
    fn bare_on(on_ubi: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let device = on_ubi.then(|| {
            std::sync::Arc::new(ubi::DirUbi::with_fixed_volumes(
                &dir.path().join("ubi"),
                4096,
            ))
        });
        let store = DeploymentStore::new(
            dir.path().join("system"),
            match &device {
                Some(device) => BootBackend::Ubi {
                    ubi: device.clone(),
                },
                None => BootBackend::Uefi {
                    esp: dir.path().join("esp"),
                },
            },
            dir.path().join("meta"),
        );
        fs::create_dir_all(&store.system).unwrap();
        fs::create_dir_all(&store.meta).unwrap();
        if let Some(device) = &device {
            let record = mica_deploy::fit_env::Record {
                id: "a".repeat(64),
                kernel_id: "c".repeat(64),
                generation: 1,
                tries_left: None,
            };
            let copy = mica_deploy::fit_env::encode(&[record], 0).unwrap();
            let volume = mica_deploy::ubi::find(device.as_ref(), "records-a")
                .unwrap()
                .unwrap();
            fs::write(
                mica_deploy::ubi::UbiDevice::path(device.as_ref(), volume.id),
                copy,
            )
            .unwrap();
        } else {
            let entries = dir.path().join("esp/loader/entries");
            fs::create_dir_all(&entries).unwrap();
            fs::write(
                entries.join(format!("mica-{}.conf", "a".repeat(64))),
                format!(
                    "title MICA\nversion 1\nsort-key mica\nefi /EFI/mica/kernels/{}.efi\n",
                    "c".repeat(64)
                ),
            )
            .unwrap();
        }
        let objects = dir.path().join("objects");
        fs::create_dir_all(&objects).unwrap();
        for fill in [1, 2, 3, 200] {
            for bytes in [image(fill), signature(fill)] {
                fs::write(objects.join(sha(&bytes)), bytes).unwrap();
            }
        }
        Self {
            _dir: dir,
            store,
            objects,
            features: vec!["micad".into(), "ui".into()],
            ubi: device,
        }
    }

    /// A device as an image leaves it: its seed set installed and current.
    fn new() -> Self {
        Self::new_on(false)
    }

    /// As [`Self::new`], on a UEFI board or on a `ubi` one.
    fn new_on(on_ubi: bool) -> Self {
        let device = Self::bare_on(on_ubi);
        device.install(&seed().0).unwrap();
        let boot = device.boot().unwrap();
        assert_eq!(device.settle(&boot), SetSettlement::Committed);
        device
    }

    fn target(&self) -> SetTarget<'_> {
        SetTarget {
            arch: "amd64",
            features: &self.features,
            root: mica_deploy::components::RootFacts {
                level: 1,
                provides: None,
            },
        }
    }

    fn install(&self, envelope: &[u8]) -> anyhow::Result<SetState> {
        self.store
            .install_set(envelope, &[key()], &self.target(), &self.objects)
    }

    /// What the runkit would compose on the next boot, spending a boot of the
    /// trial as it does, with what it reported on the way.
    fn compose(&self) -> (anyhow::Result<CoreBoot>, Vec<String>) {
        self.compose_as(&running())
    }

    /// As [`Self::compose`], booting the deployment `deployment`.
    fn compose_as(&self, deployment: &str) -> (anyhow::Result<CoreBoot>, Vec<String>) {
        let mut reported = Vec::new();
        let composed = core_set::composition(
            &self.store.system,
            &self.store.meta,
            self.ubi
                .as_deref()
                .map(|device| device as &dyn mica_deploy::ubi::UbiDevice),
            &[key()],
            &BootRoot {
                arch: "amd64",
                features: &self.features,
                root: mica_deploy::components::RootFacts {
                    level: 1,
                    provides: None,
                },
            },
            deployment,
            |refusal: &str| reported.push(refusal.to_owned()),
        )
        .map(|composition| CoreBoot {
            core_set_id: composition.id,
            pending: composition.pending,
            paired: composition.paired,
        });
        (composed, reported)
    }

    fn boot(&self) -> anyhow::Result<CoreBoot> {
        self.compose().0
    }

    fn settle(&self, boot: &CoreBoot) -> SetSettlement {
        self.settle_as(&running(), boot)
    }

    /// Settle a healthy boot of the deployment `deployment`.
    fn settle_as(&self, deployment: &str, boot: &CoreBoot) -> SetSettlement {
        self.store.settle_sets(deployment, boot, &[key()]).unwrap()
    }

    fn held(&self, relative: &str) -> bool {
        self.store.system.join(relative).exists()
    }

    /// Give the device a second deployment, `id`, whose UEFI entry has
    /// `tries` boots left (`None`: confirmed).
    fn entry(&self, id: &str, tries: Option<u8>) {
        let entries = self
            .store
            .system
            .parent()
            .unwrap()
            .join("esp/loader/entries");
        for entry in fs::read_dir(&entries).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().contains(id) {
                fs::remove_file(entry.path()).unwrap();
            }
        }
        let suffix = match tries {
            None => String::new(),
            Some(3) => "+3".to_owned(),
            Some(tries) => format!("+{tries}-{}", 3 - tries),
        };
        fs::write(
            entries.join(format!("mica-{id}{suffix}.conf")),
            format!(
                "title MICA\nversion 2\nsort-key mica\nefi /EFI/mica/kernels/{}.efi\n",
                "c".repeat(64)
            ),
        )
        .unwrap();
    }

    /// Make the pending set a pair with the deployment `deployment`, as a
    /// system update installs it: no trial of its own.
    fn pair(&self, deployment: &str) {
        set_state::end_trial(&self.store.meta).unwrap();
        set_state::write_state(
            &self.store.system,
            SetKind::Core,
            &SetState {
                pair: Some(deployment.to_owned()),
                ..self.state()
            },
        )
        .unwrap();
    }

    fn state(&self) -> SetState {
        set_state::read_state(&self.store.system, SetKind::Core).unwrap()
    }

    /// The set a system update to a deployment of root `provides`, carrying
    /// the set `image` (envelope, channel, generation), would boot with.
    fn choose(
        &self,
        image: Option<(&[u8], &str, u64)>,
        provides: Option<Value>,
        latest: Option<&[u8]>,
    ) -> anyhow::Result<CoreChoice> {
        let mut d: Value =
            serde_json::from_slice(include_bytes!("component-contracts/deployment.json")).unwrap();
        if let Some(provides) = provides {
            d["rootfs"]["provides"] = provides;
        }
        if let Some((envelope, channel, generation)) = image {
            fs::write(self.objects.join(sha(envelope)), envelope).unwrap();
            d["coreSet"] = json!({
                "id": set_id(envelope), "channel": channel, "generation": generation,
                "sha256": sha(envelope), "bytes": envelope.len(),
            });
        }
        self.store.core_choice(
            &serde_json::from_value(d).unwrap(),
            &[key()],
            &Target {
                board: "uefi-x64",
                arch: "amd64",
                product: "uefi-x64-dev",
                features: &self.features,
            },
            &self.objects,
            latest,
        )
    }

    /// Install `envelope` alone and commit it with a healthy boot.
    fn commit(&self, envelope: &[u8]) {
        self.install(envelope).unwrap();
        let boot = self.boot().unwrap();
        assert_eq!(self.settle(&boot), SetSettlement::Committed);
    }
}

/// The id of a signed set: the digest of its payload.
fn set_id(envelope: &[u8]) -> String {
    let outer: Value = serde_json::from_slice(envelope).unwrap();
    sha(&STANDARD.decode(outer["payload"].as_str().unwrap()).unwrap())
}

/// The id of the set a choice pairs, or `None` for the device's own.
fn paired(choice: CoreChoice) -> Option<String> {
    match choice {
        CoreChoice::Keep => None,
        CoreChoice::Pair { id, .. } => Some(id),
    }
}

fn image_dir(envelope: &[u8], package: &str) -> String {
    let outer: Value = serde_json::from_slice(envelope).unwrap();
    let payload: Value =
        serde_json::from_slice(&STANDARD.decode(outer["payload"].as_str().unwrap()).unwrap())
            .unwrap();
    let core = payload["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|core| core["package"] == package)
        .unwrap();
    format!("sets/core/objects/{}", core["id"].as_str().unwrap())
}

fn booted(id: &str, pending: bool) -> CoreBoot {
    CoreBoot {
        core_set_id: id.to_owned(),
        pending,
        paired: false,
    }
}

/// The deployment every device here runs.
fn running() -> String {
    "a".repeat(64)
}

/// A device with no set composes nothing and says so: a deployment names no
/// core components, so there is nothing to stand in.
#[test]
fn a_device_with_no_set_does_not_boot() {
    let device = Device::bare();
    let error = format!("{:#}", device.boot().unwrap_err());
    assert!(error.contains("holds no core set"), "{error}");
}

/// An update over the seed set: tried, committed by a healthy boot, and the
/// set it replaced released with the components only it named.
#[test]
fn a_set_is_installed_tried_committed_and_the_old_one_released() {
    let device = Device::new();
    let (first, first_id) = seed();
    assert_eq!(device.boot().unwrap(), booted(&first_id, false));

    let (second, second_id) = core_set("general", 2, "0.0.6", 2);
    let state = device.install(&second).unwrap();
    assert_eq!(state.pending.as_deref(), Some(second_id.as_str()));
    assert_eq!(
        set_state::read_trial(&device.store.meta)
            .unwrap()
            .attempts_left,
        set_state::TRIAL_ATTEMPTS
    );
    // Confirming before the reboot changes nothing: the trial is ahead.
    assert_eq!(
        device.settle(&booted(&first_id, false)),
        SetSettlement::Unchanged
    );

    let boot = device.boot().unwrap();
    assert_eq!(boot, booted(&second_id, true));
    assert_eq!(device.settle(&boot), SetSettlement::Committed);
    let state = set_state::read_state(&device.store.system, SetKind::Core).unwrap();
    assert_eq!(
        (state.current.as_deref(), state.pending),
        (Some(second_id.as_str()), None)
    );
    assert!(set_state::read_trial(&device.store.meta).is_none());
    assert!(!device.held(&format!("sets/core/{first_id}.json")));
    assert!(!device.held(&image_dir(&first, "micad")));
    assert!(device.held(&format!("sets/core/{second_id}.json")));
    assert!(device.held(&format!("{}/core.img", image_dir(&second, "micad"))));
    assert!(device.held(&format!(
        "{}/core.roothash",
        image_dir(&second, "mica-apid-ui")
    )));
    // Two sets at most, and now one.
    assert_eq!(
        fs::read_dir(device.store.system.join("sets/core"))
            .unwrap()
            .count(),
        3,
        "the current set, the state file and the objects directory"
    );
    // A deployment's installation and collection never see a set's components.
    assert!(!device.held("cores"));
}

/// A set that never reaches healthy is tried three times and then left: the
/// device composes the current set again, and the next healthy boot drops the
/// failed one with its objects.
#[test]
fn a_set_that_never_reaches_healthy_is_dropped_after_its_trial() {
    let device = Device::new();
    let (first, first_id) = seed();
    let (bad, bad_id) = core_set("general", 2, "0.0.6", 2);
    device.install(&bad).unwrap();
    for _ in 0..set_state::TRIAL_ATTEMPTS {
        // No health gate confirms: the boot failed.
        assert_eq!(device.boot().unwrap(), booted(&bad_id, true));
    }
    let boot = device.boot().unwrap();
    assert_eq!(boot, booted(&first_id, false), "the trial is spent");
    assert_eq!(device.settle(&boot), SetSettlement::Dropped);
    let state = set_state::read_state(&device.store.system, SetKind::Core).unwrap();
    assert_eq!(
        (state.current.as_deref(), state.pending),
        (Some(first_id.as_str()), None)
    );
    assert!(!device.held(&format!("sets/core/{bad_id}.json")));
    assert!(!device.held(&image_dir(&bad, "micad")));
    assert!(device.held(&format!("{}/core.img", image_dir(&first, "micad"))));
    // The dropped set can be offered again; nothing remembers it as failed.
    assert!(device.install(&bad).is_ok());
}

/// A pending set the runkit cannot use is passed over and the current set
/// composed: a damaged envelope or a missing component must not stop the
/// boot. A current set in the same state does, because nothing else is there.
#[test]
fn a_set_that_cannot_be_composed_is_passed_over_or_stops_the_boot() {
    let (first, first_id) = seed();
    let (second, second_id) = core_set("general", 2, "0.0.6", 2);

    let damaged = Device::new();
    damaged.install(&second).unwrap();
    fs::write(
        damaged
            .store
            .system
            .join(format!("sets/core/{second_id}.json")),
        "{}",
    )
    .unwrap();
    let (boot, reported) = damaged.compose();
    assert_eq!(boot.unwrap(), booted(&first_id, false));
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert!(reported[0].contains(&second_id), "{reported:?}");

    let incomplete = Device::new();
    incomplete.install(&second).unwrap();
    fs::remove_file(
        incomplete
            .store
            .system
            .join(image_dir(&second, "micad"))
            .join("core.img"),
    )
    .unwrap();
    let (boot, reported) = incomplete.compose();
    assert_eq!(boot.unwrap(), booted(&first_id, false));
    assert!(reported[0].contains(&second_id), "{reported:?}");

    // The current set's component at the wrong length rather than absent.
    fs::write(
        incomplete
            .store
            .system
            .join(image_dir(&first, "micad"))
            .join("core.img"),
        b"short",
    )
    .unwrap();
    let error = format!("{:#}", incomplete.boot().unwrap_err());
    assert!(error.contains(&first_id), "{error}");
}

/// What installation refuses, each by its reason.
#[test]
fn installation_refuses_what_the_device_could_not_run() {
    let device = Device::new();
    let (second, _) = core_set("general", 3, "0.0.6", 2);

    let refused = |device: &Device, envelope: &[u8], reason: &str| {
        let error = format!("{:#}", device.install(envelope).unwrap_err());
        assert!(error.contains(reason), "{error} does not name {reason}");
    };

    // Another architecture, an untrusted key, a root the set does not select on.
    let arm = SetTarget {
        arch: "arm64",
        ..device.target()
    };
    assert!(
        device
            .store
            .install_set(&second, &[key()], &arm, &device.objects)
            .is_err()
    );
    assert!(
        device
            .store
            .install_set(&second, &[[0; 32]], &device.target(), &device.objects)
            .is_err()
    );
    let console_only = Device {
        features: vec!["ui".into()],
        ..Device::bare()
    };
    refused(&console_only, &second, "the set does not suit this device");

    // A deployment on trial.
    device
        .store
        .save_state(&State {
            candidate: Some("a".repeat(64)),
            ..State::default()
        })
        .unwrap();
    let entries = device
        .store
        .system
        .parent()
        .unwrap()
        .join("esp/loader/entries");
    let confirmed = entries.join(format!("mica-{}.conf", "a".repeat(64)));
    let on_trial = entries.join(format!("mica-{}+3.conf", "a".repeat(64)));
    fs::rename(&confirmed, &on_trial).unwrap();
    refused(&device, &second, "a deployment is on trial");
    fs::rename(&on_trial, &confirmed).unwrap();
    device.store.save_state(&State::default()).unwrap();

    device.install(&second).unwrap();
    let (third, _) = core_set("general", 4, "0.0.7", 3);
    refused(&device, &third, "another set is pending");
    let boot = device.boot().unwrap();
    device.settle(&boot);
    refused(&device, &second, "set is installed");

    // Generations order a channel; another channel starts its own line.
    let (older, _) = core_set("general", 2, "0.0.4", 3);
    refused(&device, &older, "set generation is not newer");
    let (other_channel, _) = core_set("lts", 1, "0.0.4", 3);
    assert!(device.install(&other_channel).is_ok());
}

/// An object that is not the signed bytes is refused before anything is
/// published or any trial started.
#[test]
fn a_substituted_object_publishes_nothing() {
    let device = Device::bare();
    let (first, first_id) = seed();
    fs::write(device.objects.join(sha(&image(1))), image(9)).unwrap();
    assert!(device.install(&first).is_err());
    assert!(!device.held(&format!("sets/core/{first_id}.json")));
    assert_eq!(
        set_state::read_state(&device.store.system, SetKind::Core).unwrap(),
        SetState::default()
    );
    assert!(set_state::read_trial(&device.store.meta).is_none());
}

/// On a `ubi` board a component's image is the static volume `c-<id>`, and
/// its small files stay on SYSTEM: installed, composed from, and removed with
/// the last set that names it.
#[test]
fn a_ubi_device_holds_component_images_as_volumes() {
    let device = Device::new_on(true);
    let volumes = |envelope: &[u8], package: &str| {
        let id = image_dir(envelope, package)
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        device
            .ubi
            .as_ref()
            .unwrap()
            .names()
            .contains(&format!("c-{id}"))
    };
    let (first, _) = seed();
    assert!(volumes(&first, "micad") && volumes(&first, "mica-apid-ui"));
    assert!(!device.held(&format!("{}/core.img", image_dir(&first, "micad"))));
    assert!(device.held(&format!("{}/core.roothash.p7s", image_dir(&first, "micad"))));

    let (second, second_id) = core_set("general", 2, "0.0.6", 2);
    device.install(&second).unwrap();
    let boot = device.boot().unwrap();
    assert_eq!(boot, booted(&second_id, true));
    assert_eq!(device.settle(&boot), SetSettlement::Committed);
    assert!(!volumes(&first, "micad"), "released with its set");
    assert!(volumes(&second, "micad") && volumes(&second, "mica-apid-ui"));

    // A component whose volume is not sound is not composed.
    let id = image_dir(&second, "micad")
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    device.ubi.as_ref().unwrap().corrupt(&format!("c-{id}"));
    let error = format!("{:#}", device.boot().unwrap_err());
    assert!(error.contains("not the signed length"), "{error}");
}

/// The device's deployment `b`, a second one.
fn other() -> String {
    "b".repeat(64)
}

/// A set paired with a new deployment is composed only when that deployment
/// boots, spends no trial of its own, and is committed by that deployment's
/// healthy boot; the deployment that ran the old set is then bound to it, and
/// keeps it when it is booted again.
#[test]
fn a_paired_set_is_composed_with_its_deployment_and_the_old_one_is_bound() {
    let device = Device::new();
    let (first, first_id) = seed();
    let (second, second_id) = core_set("general", 2, "0.0.6", 2);
    device.install(&second).unwrap();
    device.pair(&other());
    device.entry(&other(), Some(3));

    // The running deployment composes the current set, and spends nothing.
    assert_eq!(device.boot().unwrap(), booted(&first_id, false));
    assert!(set_state::read_trial(&device.store.meta).is_none());
    assert_eq!(
        device.settle(&booted(&first_id, false)),
        SetSettlement::Unchanged,
        "the paired deployment is still to be tried"
    );
    let boot = device.compose_as(&other()).0.unwrap();
    assert_eq!(
        boot,
        CoreBoot {
            core_set_id: second_id.clone(),
            pending: true,
            paired: true,
        }
    );

    device.entry(&other(), None);
    assert!(device.store.sets_need_settling(&other(), &boot).unwrap());
    assert_eq!(device.settle_as(&other(), &boot), SetSettlement::Committed);
    assert_eq!(
        device.state(),
        SetState {
            current: Some(second_id.clone()),
            bound: [(running(), first_id.clone())].into(),
            ..SetState::default()
        }
    );
    // Three things named, both sets' components held.
    assert!(device.held(&format!("sets/core/{first_id}.json")));
    assert!(device.held(&format!("{}/core.img", image_dir(&first, "micad"))));
    assert!(device.held(&format!("{}/core.img", image_dir(&second, "micad"))));
    assert_eq!(device.boot().unwrap(), booted(&first_id, false));
    assert_eq!(
        device.compose_as(&other()).0.unwrap(),
        booted(&second_id, false)
    );
    assert!(
        !device
            .store
            .sets_need_settling(&other(), &booted(&second_id, false))
            .unwrap(),
        "a confirmation that changes nothing writes nothing"
    );

    // The new deployment fails later and the old one is confirmed again:
    // its own set is current once more, and the other is released.
    device.entry(&other(), Some(0));
    assert_eq!(
        device.settle(&booted(&first_id, false)),
        SetSettlement::Unchanged
    );
    assert_eq!(
        device.state(),
        SetState {
            current: Some(first_id.clone()),
            ..SetState::default()
        }
    );
    assert!(!device.held(&format!("sets/core/{second_id}.json")));
    assert!(!device.held(&image_dir(&second, "micad")));
}

/// A paired deployment that fails its trial takes its set with it: the next
/// healthy boot of the old deployment drops the pair and its objects.
#[test]
fn a_pair_whose_deployment_fails_is_dropped() {
    let device = Device::new();
    let (_, first_id) = seed();
    let (second, second_id) = core_set("general", 2, "0.0.6", 2);
    device.install(&second).unwrap();
    device.pair(&other());
    device.entry(&other(), Some(0));
    assert_eq!(
        device.settle(&booted(&first_id, false)),
        SetSettlement::Dropped
    );
    assert_eq!(
        device.state(),
        SetState {
            current: Some(first_id),
            ..SetState::default()
        }
    );
    assert!(!device.held(&format!("sets/core/{second_id}.json")));
    assert!(!device.held(&image_dir(&second, "micad")));
}

/// A set alone that becomes current binds the fallback deployment to the set
/// it ran, which is kept with its components; a binding of a deployment that
/// is gone is dropped.
#[test]
fn a_set_alone_binds_the_fallback_to_the_set_it_ran() {
    let device = Device::new();
    let (first, first_id) = seed();
    let (second, second_id) = core_set("general", 2, "0.0.6", 2);
    device.entry(&other(), None);
    device.commit(&second);
    assert_eq!(device.state().bound, [(other(), first_id.clone())].into());
    assert_eq!(
        device.compose_as(&other()).0.unwrap(),
        booted(&first_id, false)
    );
    assert!(device.held(&format!("{}/core.img", image_dir(&first, "micad"))));

    // A third set: the fallback keeps the set it was bound to, and the set
    // the running deployment left is released.
    let (third, third_id) = core_set("general", 3, "0.0.7", 3);
    device.commit(&third);
    assert_eq!(device.state().current, Some(third_id));
    assert_eq!(device.state().bound, [(other(), first_id.clone())].into());
    assert!(!device.held(&format!("sets/core/{second_id}.json")));
    assert!(!device.held(&image_dir(&second, "micad")));
    assert!(device.held(&format!("{}/core.img", image_dir(&third, "micad"))));

    // The fallback is retired: its binding and its set go.
    device.entry(&other(), Some(0));
    let boot = device.boot().unwrap();
    device.settle(&boot);
    let state = device.state();
    assert_eq!((state.pair, state.bound.len()), (None, 0));
    assert!(!device.held(&format!("sets/core/{first_id}.json")));
    assert!(!device.held(&image_dir(&first, "micad")));
}

/// The table's first row: a device on `general` below the image's
/// generation boots the new deployment with the newest general set that
/// selects on the new root, the channel's latest or else the image's.
#[test]
fn a_system_update_on_general_takes_the_newer_general_set_as_a_pair() {
    let device = Device::new();
    let (image, image_id) = core_set("general", 2, "0.0.6", 2);
    let carried = Some((image.as_slice(), "general", 2));
    assert_eq!(
        paired(device.choose(carried, None, None).unwrap()),
        Some(image_id.clone())
    );
    let (latest, latest_id) = core_set("general", 3, "0.0.7", 3);
    assert_eq!(
        paired(device.choose(carried, None, Some(&latest)).unwrap()),
        Some(latest_id)
    );

    // The latest requires more of the root than it provides: the image's set.
    let podman = json!([{"package": "mica-podman", "min": "6.1.4-7"}]);
    let old_podman = Some(json!({"mica-podman": "6.1.4-5"}));
    let (strict, strict_id) = requiring("general", 3, "0.0.7", 3, &podman);
    assert_eq!(
        paired(
            device
                .choose(carried, old_podman.clone(), Some(&strict))
                .unwrap()
        ),
        Some(image_id.clone())
    );
    // A package the root does not declare holds.
    assert_eq!(
        paired(
            device
                .choose(
                    carried,
                    Some(json!({"mica-deploy": "0.0.17-1"})),
                    Some(&strict)
                )
                .unwrap()
        ),
        Some(strict_id.clone())
    );

    // Neither selects: refused, naming both sets and the constraint.
    let (strict_image, strict_image_id) = requiring("general", 2, "0.0.6", 2, &podman);
    let error = format!(
        "{:#}",
        device
            .choose(
                Some((&strict_image, "general", 2)),
                old_podman,
                Some(&strict)
            )
            .unwrap_err()
    );
    for named in [
        strict_id.as_str(),
        strict_image_id.as_str(),
        "mica-podman >= 6.1.4-7",
        "6.1.4-5",
    ] {
        assert!(error.contains(named), "{error} does not name {named}");
    }

    // A latest older than the image's set is passed over.
    let (first, _) = seed();
    assert_eq!(
        paired(device.choose(carried, None, Some(&first)).unwrap()),
        Some(image_id.clone())
    );
    // The image's set must be at hand and be what the deployment names.
    fs::remove_file(device.objects.join(sha(&image))).unwrap();
    let mut d: Value =
        serde_json::from_slice(include_bytes!("component-contracts/deployment.json")).unwrap();
    d["coreSet"] = json!({"id": image_id, "channel": "general", "generation": 2,
        "sha256": sha(&image), "bytes": image.len()});
    let error = format!(
        "{:#}",
        device
            .store
            .core_choice(
                &serde_json::from_value(d).unwrap(),
                &[key()],
                &Target {
                    board: "uefi-x64",
                    arch: "amd64",
                    product: "uefi-x64-dev",
                    features: &device.features,
                },
                &device.objects,
                None,
            )
            .unwrap_err()
    );
    assert!(error.contains("not at hand"), "{error}");
    let error = format!(
        "{:#}",
        device
            .choose(Some((&image, "general", 5)), None, None)
            .unwrap_err()
    );
    assert!(error.contains("differs from what it names"), "{error}");
}

/// The table's other rows: the device's set is kept, and must select on the
/// new root, when it is of the image's generation or newer, of another
/// channel, or when the deployment names no set.
#[test]
fn a_system_update_never_downgrades_core_or_moves_it_off_its_channel() {
    let podman = json!([{"package": "mica-podman", "min": "6.1.4-7"}]);
    let old_podman = || Some(json!({"mica-podman": "6.1.4-5"}));
    let (image, _) = core_set("general", 2, "0.0.6", 2);
    let carried = Some((image.as_slice(), "general", 2));

    // Of the image's generation, or newer.
    let device = Device::new();
    let (current, current_id) = requiring("general", 2, "0.0.6", 3, &podman);
    device.commit(&current);
    assert!(paired(device.choose(carried, None, None).unwrap()).is_none());
    let (newer, _) = core_set("general", 5, "0.0.9", 3);
    assert!(
        paired(device.choose(carried, None, Some(&newer)).unwrap()).is_none(),
        "the latest is not taken by a device already at the image's generation"
    );
    let error = format!(
        "{:#}",
        device.choose(carried, old_podman(), None).unwrap_err()
    );
    assert!(
        error.contains(&current_id) && error.contains("root requirement does not hold"),
        "{error}"
    );

    // Another channel, below the image's generation.
    let device = Device::new();
    let (lts, lts_id) = requiring("lts", 1, "0.0.5", 3, &podman);
    device.commit(&lts);
    assert!(paired(device.choose(carried, None, None).unwrap()).is_none());
    let error = format!(
        "{:#}",
        device.choose(carried, old_podman(), None).unwrap_err()
    );
    assert!(error.contains(&lts_id), "{error}");

    // No set named by the deployment.
    let device = Device::new();
    assert!(paired(device.choose(None, None, None).unwrap()).is_none());
    let (strict, strict_id) = requiring("general", 2, "0.0.6", 3, &podman);
    device.commit(&strict);
    let error = format!("{:#}", device.choose(None, old_podman(), None).unwrap_err());
    assert!(error.contains(&strict_id), "{error}");
}
