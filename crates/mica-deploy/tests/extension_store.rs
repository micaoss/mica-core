//! Driver extensions on SYSTEM: installed for the running kernel as a change
//! on trial, committed by a healthy boot or dropped when the trial is spent,
//! replaced by name, removed, collected, and all gone with the owner's
//! certificates at the end of a factory reset.
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
    deployments::{BootBackend, DeploymentStore, SetSettlement},
    extensions,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};
use tempfile::TempDir;

fn sha(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}
fn signer() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[13; 32]).unwrap()
}
fn key() -> [u8; 32] {
    signer().public_key().as_ref().try_into().unwrap()
}

fn envelope(payload: &[u8]) -> Vec<u8> {
    let signer = signer();
    format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha(signer.public_key().as_ref()),
        STANDARD.encode(payload),
        STANDARD.encode(signer.sign(payload).as_ref())
    )
    .into_bytes()
}

/// An extension's image and signature: the smallest verity geometry.
fn image(fill: u8) -> Vec<u8> {
    vec![fill; 12288]
}
fn signature(fill: u8) -> Vec<u8> {
    vec![fill; 900]
}

struct Device {
    _dir: TempDir,
    store: DeploymentStore,
    objects: PathBuf,
    /// The id of the kernel the one deployment runs.
    kernel: String,
    /// The one deployment, which runs.
    deployment: String,
    ubi: Option<std::sync::Arc<ubi::DirUbi>>,
}

impl Device {
    /// A device with one confirmed deployment, whose kernel has `features`.
    fn new(on_ubi: bool, features: &[&str]) -> Self {
        let dir = TempDir::new().unwrap();
        let mut d: Value =
            serde_json::from_slice(include_bytes!("component-contracts/deployment.json")).unwrap();
        let kernel = d["kernel"].as_object_mut().unwrap();
        kernel.remove("id");
        if !features.is_empty() {
            kernel.insert("features".into(), json!(features));
        }
        let id = component_id(&d["kernel"]).unwrap();
        d["kernel"]["id"] = json!(id);
        let deployment = component_id(&d).unwrap();
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
        fs::create_dir_all(store.system.join("deployments")).unwrap();
        fs::create_dir_all(&store.meta).unwrap();
        fs::write(
            store.system.join(format!("deployments/{deployment}.json")),
            envelope(&serde_json::to_vec(&d).unwrap()),
        )
        .unwrap();
        if let Some(device) = &device {
            let record = mica_deploy::fit_env::Record {
                id: deployment.clone(),
                kernel_id: id.clone(),
                generation: 1,
                tries_left: None,
            };
            let volume = mica_deploy::ubi::find(device.as_ref(), "records-a")
                .unwrap()
                .unwrap();
            fs::write(
                mica_deploy::ubi::UbiDevice::path(device.as_ref(), volume.id),
                mica_deploy::fit_env::encode(&[record], 0).unwrap(),
            )
            .unwrap();
        } else {
            let entries = dir.path().join("esp/loader/entries");
            fs::create_dir_all(&entries).unwrap();
            fs::write(
                entries.join(format!("mica-{deployment}.conf")),
                format!("title MICA\nversion 1\nsort-key mica\nefi /EFI/mica/kernels/{id}.efi\n"),
            )
            .unwrap();
        }
        let objects = dir.path().join("objects");
        fs::create_dir_all(&objects).unwrap();
        Self {
            _dir: dir,
            store,
            objects,
            kernel: id,
            deployment,
            ubi: device,
        }
    }

    /// The descriptor of the extension `name` at `version`, built for
    /// `kernel` on `arch`, its objects of fill `fill` staged.
    fn descriptor(&self, name: &str, version: &str, fill: u8, kernel: &str, arch: &str) -> Vec<u8> {
        fs::write(self.objects.join("ext.img"), image(fill)).unwrap();
        fs::write(self.objects.join("ext.roothash.p7s"), signature(fill)).unwrap();
        serde_json::to_vec(&json!({
            "arch": arch,
            "content": {
                "image": {"bytes": 12288, "sha256": sha(&image(fill))},
                "rootHash": "22".repeat(32),
                "signature": {"bytes": 900, "sha256": sha(&signature(fill))},
                "verity": {"algorithm": "sha256", "dataBlockSize": 4096, "dataBlocks": 2,
                    "hashBlockSize": 4096, "hashOffset": 8192, "salt": "44".repeat(32),
                    "version": 1},
            },
            "kernel": kernel,
            "name": name,
            "schema": "mica/driver-ext-descriptor/v1",
            "version": version,
        }))
        .unwrap()
    }

    fn install(&self, name: &str, version: &str, fill: u8) -> anyhow::Result<String> {
        let descriptor = self.descriptor(name, version, fill, &self.kernel, "amd64");
        self.install_bytes(&descriptor)
    }

    fn install_bytes(&self, descriptor: &[u8]) -> anyhow::Result<String> {
        self.store.install_extension(
            descriptor,
            &self.objects.join("ext.img"),
            &self.objects.join("ext.roothash.p7s"),
            &[key()],
            &self.deployment,
        )
    }

    fn remove(&self, name: &str, kernel: Option<&str>) -> anyhow::Result<usize> {
        self.store
            .remove_extension(name, kernel, &[key()], &self.deployment)
    }

    /// Confirm a boot that mapped the pending change, alone.
    fn commit(&self) -> SetSettlement {
        let id = extensions::read_state(&self.store.system)
            .unwrap()
            .pending_id()
            .unwrap();
        self.store
            .settle_extensions(
                &self.deployment,
                Some(&extensions::BootTrial { paired: false, id }),
                &[key()],
            )
            .unwrap()
    }

    fn builds(&self) -> Vec<(String, String)> {
        extensions::read_state(&self.store.system)
            .unwrap()
            .builds
            .get(&self.kernel)
            .map(|builds| builds.clone().into_iter().collect())
            .unwrap_or_default()
    }

    /// Whether the build `id`'s image is held.
    fn held(&self, id: &str) -> bool {
        match &self.ubi {
            Some(ubi) => ubi.names().contains(&format!("x-{id}")),
            None => extensions::object_dir(&self.store.system, id)
                .join("ext.img")
                .exists(),
        }
    }
}

#[test]
fn an_extension_is_installed_replaced_and_collected() {
    for on_ubi in [false, true] {
        let device = Device::new(on_ubi, &["driver-ext"]);
        let first = device.install("rtl8125", "1.0", 1).unwrap();
        assert!(device.builds().is_empty(), "on trial");
        assert_eq!(device.commit(), SetSettlement::Committed);
        assert_eq!(device.builds(), [("rtl8125".to_owned(), first.clone())]);
        assert!(device.held(&first));
        let descriptor = extensions::object_dir(&device.store.system, &first).join("ext.json");
        assert!(descriptor.is_file());
        assert_eq!(
            device.install("rtl8125", "1.0", 1).unwrap(),
            first,
            "installing it again changes nothing"
        );

        assert!(
            extensions::read_state(&device.store.system)
                .unwrap()
                .pending
                .is_none()
        );
        let second = device.install("rtl8125", "1.1", 2).unwrap();
        assert_ne!(second, first);
        assert_eq!(device.commit(), SetSettlement::Committed);
        assert_eq!(device.builds(), [("rtl8125".to_owned(), second.clone())]);
        // The build this boot mapped stays until it is not mapped.
        let mapped = BTreeSet::from([first.clone()]);
        assert_eq!(
            device.store.collect_extensions(&mapped, &[key()]).unwrap(),
            0
        );
        assert!(device.held(&first));
        assert_eq!(
            device
                .store
                .collect_extensions(&BTreeSet::new(), &[key()])
                .unwrap(),
            1
        );
        assert!(!device.held(&first) && device.held(&second), "{on_ubi}");

        assert_eq!(device.remove("other", None).unwrap(), 0);
        assert!(device.remove("rtl8125", Some(&"0".repeat(64))).is_err());
        assert_eq!(device.remove("rtl8125", None).unwrap(), 1);
        assert_eq!(device.commit(), SetSettlement::Committed);
        assert!(device.builds().is_empty());
        assert!(
            !extensions::dir(&device.store.system)
                .join("state.json")
                .exists()
        );
        assert!(
            device.held(&second),
            "removed from the state, then collected"
        );
        device
            .store
            .collect_extensions(&BTreeSet::new(), &[key()])
            .unwrap();
        assert!(!device.held(&second));
    }
}

#[test]
fn an_extension_is_refused_for_a_kernel_the_device_cannot_run_it_on() {
    let device = Device::new(false, &["driver-ext"]);
    let other = "0".repeat(64);
    let error = device
        .install_bytes(&device.descriptor("rtl8125", "1.0", 1, &other, "amd64"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("only for the running kernel"), "{error}");
    let kernel = device.kernel.clone();
    let error = device
        .install_bytes(&device.descriptor("rtl8125", "1.0", 1, &kernel, "arm64"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("another architecture"), "{error}");

    let device = Device::new(false, &[]);
    let error = device.install("rtl8125", "1.0", 1).unwrap_err().to_string();
    assert!(error.contains("does not map driver extensions"), "{error}");
    assert!(device.builds().is_empty());
}

#[test]
fn objects_that_are_not_the_descriptors_install_nothing() {
    let device = Device::new(false, &["driver-ext"]);
    let descriptor = device.descriptor("rtl8125", "1.0", 1, &device.kernel.clone(), "amd64");
    fs::write(device.objects.join("ext.img"), image(9)).unwrap();
    assert!(device.install_bytes(&descriptor).is_err());
    assert!(device.builds().is_empty());
    assert!(device.install_bytes(b"{}").is_err());
}

#[test]
fn a_kernel_runs_a_bounded_number_of_extensions() {
    let device = Device::new(false, &["driver-ext"]);
    for n in 0..extensions::MAX_EXTENSIONS {
        device.install(&format!("ext{n}"), "1.0", 1).unwrap();
    }
    let error = device
        .install("one-more", "1.0", 1)
        .unwrap_err()
        .to_string();
    assert!(error.contains("too many"), "{error}");
    // Replacing one by name is not one more.
    device.install("ext0", "2.0", 2).unwrap();
}

#[test]
fn builds_for_a_kernel_no_deployment_runs_are_collected() {
    let device = Device::new(false, &["driver-ext"]);
    let id = device.install("rtl8125", "1.0", 1).unwrap();
    device.commit();
    let mut state = extensions::read_state(&device.store.system).unwrap();
    let builds = state.builds.remove(&device.kernel).unwrap();
    state.builds.insert("0".repeat(64), builds);
    extensions::write_state(&device.store.system, &state).unwrap();
    assert_eq!(
        device
            .store
            .collect_extensions(&BTreeSet::new(), &[key()])
            .unwrap(),
        1
    );
    assert!(!device.held(&id));
    assert_eq!(
        extensions::read_state(&device.store.system).unwrap(),
        extensions::State::default()
    );
}

#[test]
fn a_factory_reset_removes_every_extension_and_enrolled_certificate() {
    for on_ubi in [false, true] {
        let device = Device::new(on_ubi, &["driver-ext"]);
        let a = device.install("a", "1.0", 1).unwrap();
        let b = device.install("b", "1.0", 2).unwrap();
        let certs = device
            .store
            .system
            .join(mica_deploy::boot::trust::OWNER_CERTS_DIR);
        fs::create_dir_all(&certs).unwrap();
        fs::write(certs.join(format!("{}.der", "1".repeat(64))), b"der").unwrap();
        assert_eq!(device.store.remove_all_extensions().unwrap(), 2);
        assert!(!device.held(&a) && !device.held(&b));
        assert!(!extensions::dir(&device.store.system).exists());
        assert!(!certs.exists());
        assert_eq!(device.store.remove_all_extensions().unwrap(), 0, "again");
    }
}

/// A change alone is tried for its boots, each spent before it is mapped; a
/// healthy boot that did not map it changes nothing while boots are left,
/// and once they are spent nothing maps it, the change is dropped and the
/// confirmed builds stay.
#[test]
fn a_change_whose_trial_is_spent_is_dropped() {
    let device = Device::new(false, &["driver-ext"]);
    let first = device.install("rtl8125", "1.0", 1).unwrap();
    device.commit();
    let second = device.install("rtl8125", "1.1", 2).unwrap();
    let state = extensions::read_state(&device.store.system).unwrap();
    let spend = |id: &str| {
        mica_deploy::set_state::spend_attempt(
            &device.store.meta,
            mica_deploy::set_state::TrialKind::Extensions,
            id,
        )
    };
    for boot in 1..=mica_deploy::set_state::TRIAL_ATTEMPTS {
        let (builds, trial) =
            extensions::boot_choice(&state, &device.deployment, &device.kernel, spend);
        assert_eq!(builds["rtl8125"], second);
        assert!(!trial.unwrap().paired);
        if boot < mica_deploy::set_state::TRIAL_ATTEMPTS {
            assert_eq!(
                device
                    .store
                    .settle_extensions(&device.deployment, None, &[key()])
                    .unwrap(),
                SetSettlement::Unchanged,
                "a boot that did not map it, with boots left"
            );
        }
    }
    let (builds, trial) =
        extensions::boot_choice(&state, &device.deployment, &device.kernel, spend);
    assert_eq!((builds["rtl8125"].clone(), trial), (first.clone(), None));
    assert!(
        device
            .store
            .extensions_need_settling(&device.deployment, None, &[key()])
            .unwrap()
    );
    assert_eq!(
        device
            .store
            .settle_extensions(&device.deployment, None, &[key()])
            .unwrap(),
        SetSettlement::Dropped
    );
    assert_eq!(device.builds(), [("rtl8125".to_owned(), first)]);
    assert!(mica_deploy::set_state::read_trial(&device.store.meta).is_none());
    assert_eq!(
        device
            .store
            .collect_extensions(&BTreeSet::new(), &[key()])
            .unwrap(),
        1
    );
    assert!(!device.held(&second));
}

/// One trial at a time: a change waits while a core set is on trial.
#[test]
fn a_change_waits_while_a_core_set_is_on_trial() {
    let device = Device::new(false, &["driver-ext"]);
    let sets = device.store.system.join("sets/core");
    fs::create_dir_all(&sets).unwrap();
    fs::write(
        sets.join("state.json"),
        format!(
            "{{\"current\":\"{}\",\"pending\":\"{}\"}}",
            "1".repeat(64),
            "2".repeat(64)
        ),
    )
    .unwrap();
    let error = device.install("rtl8125", "1.0", 1).unwrap_err().to_string();
    assert!(error.contains("a core set is on trial"), "{error}");
    assert!(device.remove("rtl8125", None).is_err());
}
