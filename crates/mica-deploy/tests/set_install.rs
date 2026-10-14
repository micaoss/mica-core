//! A core set from installation to the end of its trial: published beside the
//! current one (every device has one: its image's seed set), tried by the runkit while its trial has boots left, then
//! committed by a healthy boot or dropped.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    components::component_id,
    core_set::{self, BootRoot, CoreBoot},
    deployments::{BootBackend, DeploymentStore, SetSettlement, SetTarget, State},
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

fn component(package: &str, version: &str, feature: &str, needs: Value, fill: u8) -> Value {
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
    core["id"] = json!(component_id(&core).unwrap());
    core
}

/// A signed set of `micad` (image `fill`) and a console that needs it (image
/// 200), with its id.
fn core_set(channel: &str, generation: u64, version: &str, fill: u8) -> (Vec<u8>, String) {
    let payload = serde_json::to_vec(&json!({
        "schema": "mica/set/v1", "kind": "core", "channel": channel, "arch": "amd64",
        "generation": generation, "version": version,
        "components": [
            component("mica-apid-ui", version, "ui",
                json!([{"package": "micad", "min": version, "max": version}]), 200),
            component("micad", version, "micad", json!([]), fill),
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
}

/// The seed set an image carries: general generation 1, micad's image 1.
fn seed() -> (Vec<u8>, String) {
    core_set("general", 1, "0.0.5", 1)
}

impl Device {
    /// A device with one confirmed deployment, no core set yet, and every
    /// object a test's sets name already downloaded.
    fn bare() -> Self {
        let dir = TempDir::new().unwrap();
        let store = DeploymentStore::new(
            dir.path().join("system"),
            BootBackend::Uefi {
                esp: dir.path().join("esp"),
            },
            dir.path().join("meta"),
        );
        let entries = dir.path().join("esp/loader/entries");
        fs::create_dir_all(&entries).unwrap();
        fs::create_dir_all(&store.system).unwrap();
        fs::create_dir_all(&store.meta).unwrap();
        fs::write(
            entries.join(format!("mica-{}.conf", "a".repeat(64))),
            format!(
                "title MICA\nversion 1\nsort-key mica\nefi /EFI/mica/kernels/{}.efi\n",
                "c".repeat(64)
            ),
        )
        .unwrap();
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
        }
    }

    /// A device as an image leaves it: its seed set installed and current.
    fn new() -> Self {
        let device = Self::bare();
        device.install(&seed().0).unwrap();
        let boot = device.boot().unwrap();
        assert_eq!(device.settle(&boot), SetSettlement::Committed);
        device
    }

    fn target(&self) -> SetTarget<'_> {
        SetTarget {
            arch: "amd64",
            features: &self.features,
            root_level: 1,
        }
    }

    fn install(&self, envelope: &[u8]) -> anyhow::Result<SetState> {
        self.store
            .install_set(envelope, &[key()], &self.target(), &self.objects)
    }

    /// What the runkit would compose on the next boot, spending a boot of the
    /// trial as it does, with what it reported on the way.
    fn compose(&self) -> (anyhow::Result<CoreBoot>, Vec<String>) {
        let mut reported = Vec::new();
        let composed = core_set::composition(
            &self.store.system,
            &self.store.meta,
            &[key()],
            &BootRoot {
                arch: "amd64",
                features: &self.features,
                level: 1,
            },
            |refusal: &str| reported.push(refusal.to_owned()),
        )
        .map(|composition| CoreBoot {
            core_set_id: composition.id,
            pending: composition.pending,
        });
        (composed, reported)
    }

    fn boot(&self) -> anyhow::Result<CoreBoot> {
        self.compose().0
    }

    fn settle(&self, boot: &CoreBoot) -> SetSettlement {
        self.store.settle_sets(boot, &[key()]).unwrap()
    }

    fn held(&self, relative: &str) -> bool {
        self.store.system.join(relative).exists()
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
    }
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
