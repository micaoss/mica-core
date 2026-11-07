//! Process-interruption and ENOSPC acceptance for the real transaction implementation.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../support/boards.rs"]
mod boards;
#[path = "../support/ubi.rs"]
mod ubi;

use aws_lc_rs::{
    digest,
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    boot::BootKind,
    components::{authenticate_deployment, component_id},
    deployments::{BootBackend, BootReceipt, DeploymentStore, State, Target},
    fit_env::{Record, encode},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

mod capacity;
mod transactions;
mod trials;

/// Where a device keeps its deployments: files with UEFI entries, files with
/// FIT records in a partition, or UBI volumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Uefi,
    Fit,
    Ubi,
}

pub(crate) const KINDS: [Kind; 3] = [Kind::Uefi, Kind::Fit, Kind::Ubi];

impl Kind {
    /// Whether the device boots FIT records.
    fn fit(self) -> bool {
        self != Self::Uefi
    }

    fn named(name: &str) -> Self {
        KINDS
            .into_iter()
            .find(|kind| format!("{kind:?}") == name)
            .unwrap()
    }
}

fn store(root: &Path, kind: Kind) -> DeploymentStore {
    DeploymentStore::new(
        root.join("store/system"),
        match kind {
            Kind::Fit => BootBackend::Fit {
                layout: boards::layout("cx3576"),
                firmware: root.join("store/firmware.img"),
            },
            Kind::Uefi => BootBackend::Uefi {
                esp: root.join("store/esp"),
            },
            Kind::Ubi => BootBackend::Ubi {
                ubi: std::sync::Arc::new(ubi::DirUbi::open_at(&root.join("store/ubi"))),
            },
        },
        root.join("store/meta"),
    )
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}
fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn receipt(d: &Value, kind: Kind) -> BootReceipt {
    let id = component_id(d).unwrap();
    BootReceipt {
        backend: if kind.fit() {
            BootKind::UbootFit
        } else {
            BootKind::Uefi
        },
        entry: if kind.fit() {
            format!("fit:{id}")
        } else {
            format!("mica-{id}.conf")
        },
        deployment_id: id,
        kernel_id: d["kernel"]["id"].as_str().unwrap().into(),
        rootfs_id: d["rootfs"]["id"].as_str().unwrap().into(),
        content_verified: true,
        boot_verified: true,
        secure_boot: !kind.fit(),
    }
}
fn paths(root: &Path, d: &Value, kind: Kind) -> Vec<PathBuf> {
    let kernel = d["kernel"]["id"].as_str().unwrap();
    let rootfs = d["rootfs"]["id"].as_str().unwrap();
    vec![
        if kind.fit() {
            root.join(format!("store/system/kernels/{kernel}/boot.itb"))
        } else {
            root.join(format!("store/esp/EFI/mica/kernels/{kernel}.efi"))
        },
        root.join(format!("store/system/kernels/{kernel}/support.img")),
        root.join(format!(
            "store/system/kernels/{kernel}/support.roothash.p7s"
        )),
        root.join(format!("store/system/roots/{rootfs}/rootfs.img")),
        root.join(format!("store/system/roots/{rootfs}/rootfs.roothash.p7s")),
    ]
}
fn seed(root: &Path, kind: Kind, operation: &str) {
    if kind == Kind::Ubi {
        ubi::DirUbi::with_fixed_volumes(&root.join("store/ubi"), 4096);
    }
    let store = store(root, kind);
    let key = Ed25519KeyPair::from_pkcs8(
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref(),
    )
    .unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let mut deployments = Vec::new();
    let mut envelopes = Vec::new();
    for generation in 1..=3 {
        let payload = vec![
            if generation == 3 && operation == "reuse" {
                40
            } else {
                39 + generation as u8
            };
            12288
        ];
        let artifact = json!({"bytes":payload.len(),"sha256":hash(&payload)});
        let mut d: Value =
            serde_json::from_slice(include_bytes!("../component-contracts/deployment.json"))
                .unwrap();
        if kind.fit() {
            d["board"] = json!("cx3576");
            d["arch"] = json!("arm64");
            d["kernel"]["board"] = json!("cx3576");
            d["kernel"]["arch"] = json!("arm64");
            d["kernel"]["boot"]["format"] = json!("fit");
            d["rootfs"]["arch"] = json!("arm64");
        }
        for path in [
            "/kernel/boot/artifact",
            "/kernel/support/image",
            "/kernel/support/signature",
            "/rootfs/content/image",
            "/rootfs/content/signature",
        ] {
            *d.pointer_mut(path).unwrap() = artifact.clone();
        }
        // An import writes into volumes only what no file of SYSTEM shares,
        // so the candidate's signatures differ from its images there.
        if generation == 3 && operation == "import" {
            let signature = vec![7_u8; 512];
            for path in ["/kernel/support/signature", "/rootfs/content/signature"] {
                *d.pointer_mut(path).unwrap() =
                    json!({"bytes": signature.len(), "sha256": hash(&signature)});
            }
            write(&root.join("objects").join(hash(&signature)), &signature);
            let mut archive = Vec::new();
            for object in [&payload, &signature] {
                archive.extend(hash(object).as_bytes());
                archive.extend((object.len() as u64).to_be_bytes());
                archive.extend(object.as_slice());
            }
            write(&root.join("candidate-objects"), &archive);
        }
        d["generation"] = json!(generation);
        if operation.starts_with("ext-") {
            // A kernel whose runkit maps driver extensions.
            d["kernel"]["features"] = json!(["driver-ext"]);
        }
        d["kernel"]["id"] = json!(component_id(&d["kernel"]).unwrap());
        d["rootfs"]["id"] = json!(component_id(&d["rootfs"]).unwrap());
        if generation == 3 && operation == "pair" {
            // The candidate's image carries a newer general set than the
            // device's, with the device's one component at a new version.
            let (envelope, id) = core_set(&key, &public, &deployments[0], 2, "1.0.1");
            write(&root.join("objects").join(hash(&envelope)), &envelope);
            write(&root.join("paired-set"), id.as_bytes());
            d["coreSet"] = json!({"id": id, "channel": "general", "generation": 2,
                "sha256": hash(&envelope), "bytes": envelope.len()});
        }
        let bytes = serde_json::to_vec(&d).unwrap();
        let envelope = format!(
            "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
            hash(&public), STANDARD.encode(&bytes), STANDARD.encode(key.sign(&bytes))
        ).into_bytes();
        write(&root.join("objects").join(hash(&payload)), &payload);
        if generation < 3 {
            if kind == Kind::Ubi {
                let device = ubi::DirUbi::open_at(&root.join("store/ubi"));
                for (place, _) in
                    store.object_paths(&authenticate_deployment(&envelope, &[public]).unwrap())
                {
                    match place {
                        mica_deploy::deployments::Place::File(path) => write(&path, &payload),
                        mica_deploy::deployments::Place::Volume(name) => {
                            if mica_deploy::ubi::find(&device, &name).unwrap().is_none() {
                                device.put_static(&name, &payload);
                            }
                        }
                    }
                }
            } else {
                for path in paths(root, &d, kind) {
                    write(&path, &payload);
                }
            }
            for (namespace, name, component) in [
                ("kernels", "support", "kernel"),
                ("roots", "rootfs", "rootfs"),
            ] {
                let id = d[component]["id"].as_str().unwrap();
                write(
                    &store
                        .system
                        .join(format!("{namespace}/{id}/{name}.roothash")),
                    "a".repeat(64).as_bytes(),
                );
            }
            write(
                &store
                    .system
                    .join(format!("deployments/{}.json", component_id(&d).unwrap())),
                &envelope,
            );
        }
        deployments.push(d);
        envelopes.push(envelope);
    }
    // The device's core set: a deployment is installed only over one that
    // selects on its root. One component, signed with the same key, named by
    // the state as current; nothing here composes it, so its objects are not
    // on SYSTEM.
    {
        let (envelope, id) = core_set(&key, &public, &deployments[0], 1, "1.0.0");
        write(
            &store.system.join(format!("sets/core/{id}.json")),
            &envelope,
        );
        write(
            &store.system.join("sets/core/state.json"),
            format!("{{\"current\":\"{id}\"}}").as_bytes(),
        );
    }
    let records: Vec<_> = deployments[..2]
        .iter()
        .rev()
        .map(|d| Record {
            id: component_id(d).unwrap(),
            kernel_id: d["kernel"]["id"].as_str().unwrap().into(),
            generation: d["generation"].as_u64().unwrap(),
            tries_left: None,
        })
        .collect();
    if kind == Kind::Ubi {
        let device = ubi::DirUbi::open_at(&root.join("store/ubi"));
        for (i, name) in mica_deploy::ubi::RECORDS.into_iter().enumerate() {
            let volume = mica_deploy::ubi::find(&device, name).unwrap().unwrap();
            fs::write(
                mica_deploy::ubi::UbiDevice::path(&device, volume.id),
                encode(&records, i as u8).unwrap(),
            )
            .unwrap();
        }
    } else if kind.fit() {
        let mut file = fs::File::create(root.join("store/firmware.img")).unwrap();
        file.set_len(18 * 1048576 - 32768).unwrap();
        for (i, offset) in boards::layout("cx3576").offsets().into_iter().enumerate() {
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&encode(&records, i as u8).unwrap()).unwrap();
        }
    } else {
        for record in &records {
            write(
                &root.join(format!("store/esp/loader/entries/mica-{}.conf", record.id)),
                format!(
                    "title MICA\nversion {}\nsort-key mica\nefi /EFI/mica/kernels/{}.efi\n",
                    record.generation, record.kernel_id
                )
                .as_bytes(),
            );
        }
    }
    fs::create_dir_all(&store.meta).unwrap();
    store
        .save_state(&State {
            current: Some(records[0].id.clone()),
            fallback: Some(records[1].id.clone()),
            highest_generation: 2,
            ..State::default()
        })
        .unwrap();
    write(&root.join("public"), &public);
    write(&root.join("candidate.json"), &envelopes[2]);
    write(
        &root.join("candidate-receipt.json"),
        &serde_json::to_vec(&receipt(&deployments[2], kind)).unwrap(),
    );
    write(&root.join("retained-id"), records[0].id.as_bytes());
    write(&root.join("retired-id"), records[1].id.as_bytes());
    write(
        &root.join("current-receipt.json"),
        &serde_json::to_vec(&receipt(&deployments[1], kind)).unwrap(),
    );
    if operation == "import" {
        let envelope = &envelopes[2];
        let mut archive = b"MICAUPD1".to_vec();
        archive.extend((envelope.len() as u32).to_be_bytes());
        archive.extend(envelope);
        archive.extend(2_u32.to_be_bytes());
        archive.extend(fs::read(root.join("candidate-objects")).unwrap());
        write(&root.join("candidate.archive"), &archive);
    }
    if !["install", "reuse", "import", "ext-install", "ext-pair"].contains(&operation) {
        store
            .install(
                &envelopes[2],
                &[public],
                &Target {
                    board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                    arch: if kind.fit() { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                    features: &[],
                },
                &root.join("objects"),
                &serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                    .unwrap(),
                None,
            )
            .unwrap();
    }
    if !["install", "reuse", "import", "ext-install", "ext-pair"].contains(&operation) {
        if store.boot.is_fit() {
            let mut environment = store.boot.records().unwrap();
            environment.records[0].tries_left = Some(2);
            store.boot.save_records(&environment).unwrap();
        } else {
            let id = component_id(&deployments[2]).unwrap();
            fs::rename(
                root.join(format!("store/esp/loader/entries/mica-{id}+3.conf")),
                root.join(format!("store/esp/loader/entries/mica-{id}+2-1.conf")),
            )
            .unwrap();
        }
    }
    if operation.starts_with("reset-") {
        // What DATA's meta/ holds beside the deployment state, for the
        // snapshot a staged reset keeps.
        write(&store.meta.join("catalog.json"), b"{\"checkpoints\":[]}");
        write(&store.meta.join("firmware.json"), b"{\"receipt\":true}");
    }
    if operation == "reset-finish" {
        // The candidate is confirmed and runs; the reset to it is staged, and
        // the runkit has made DATA again at this boot.
        let running = receipt(&deployments[2], kind);
        store.confirm(&running).unwrap();
        mica_deploy::factory_reset::stage(
            &store.system,
            &store.meta,
            &reset_request(&running.deployment_id),
        )
        .unwrap();
        write(&root.join("retained-id"), running.deployment_id.as_bytes());
    }
    if operation.starts_with("ext-") {
        // The running kernel runs a confirmed build of the extension. Its
        // next build is downloaded beside it: for the same kernel, or for the
        // kernel of the system update it comes with, its objects by digest.
        let kernel = deployments[1]["kernel"]["id"].as_str().unwrap();
        let arch = deployments[1]["kernel"]["arch"].as_str().unwrap();
        let old = extension(root, "ext-1", kernel, arch, 50);
        let id = hash(&old);
        let directory = mica_deploy::extensions::object_dir(&store.system, &id);
        fs::create_dir_all(&directory).unwrap();
        for file in ["ext.json", "ext.roothash.p7s", "ext.img"] {
            let bytes = fs::read(root.join("ext-1").join(file)).unwrap();
            match kind {
                Kind::Ubi if file == "ext.img" => {
                    ubi::DirUbi::open_at(&root.join("store/ubi"))
                        .put_static(&format!("x-{id}"), &bytes);
                }
                _ => write(&directory.join(file), &bytes),
            }
        }
        mica_deploy::extensions::write_state(
            &store.system,
            &mica_deploy::extensions::State {
                builds: [(
                    kernel.to_owned(),
                    [("rtl8125".to_owned(), id.clone())].into(),
                )]
                .into(),
                ..Default::default()
            },
        )
        .unwrap();
        write(&root.join("ext-old-id"), id.as_bytes());
        write(&root.join("ext-kernel"), kernel.as_bytes());
        let next = if operation == "ext-install" {
            kernel
        } else {
            deployments[2]["kernel"]["id"].as_str().unwrap()
        };
        assert!((next != kernel) == (operation != "ext-install"));
        let new = extension(root, "ext-2", next, arch, 51);
        for file in ["ext.img", "ext.roothash.p7s"] {
            let bytes = fs::read(root.join("ext-2").join(file)).unwrap();
            write(&root.join("objects").join(hash(&bytes)), &bytes);
        }
        write(&root.join("ext-new-id"), hash(&new).as_bytes());
        write(&root.join("ext-next-kernel"), next.as_bytes());
        write(
            &root.join("candidate-id"),
            component_id(&deployments[2]).unwrap().as_bytes(),
        );
        write(
            &root.join("fallback-kernel"),
            deployments[0]["kernel"]["id"].as_str().unwrap().as_bytes(),
        );
    }
    if operation == "gc" {
        store.confirm(&receipt(&deployments[2], kind)).unwrap();
        if kind == Kind::Ubi {
            ubi::DirUbi::open_at(&root.join("store/ubi"))
                .put_static(&format!("r-{}", "f".repeat(64)), b"unreachable");
            ubi::DirUbi::open_at(&root.join("store/ubi"))
                .put_static(&format!("p.k-{}", "f".repeat(64)), b"interrupted");
            // The small files of an unreachable root stay on SYSTEM.
            write(
                &store
                    .system
                    .join(format!("roots/{}/rootfs.roothash", "f".repeat(64))),
                b"unreachable",
            );
        } else {
            write(
                &store
                    .system
                    .join(format!("roots/{}/rootfs.img", "f".repeat(64))),
                b"unreachable",
            );
        }
    }
}
/// The objects of a build of the driver extension `rtl8125` for `kernel`,
/// on `arch`, of fill `fill`, in `<root>/<dir>`, and its descriptor, also
/// written there.
fn extension(root: &Path, dir: &str, kernel: &str, arch: &str, fill: u8) -> Vec<u8> {
    let (image, signature) = (vec![fill; 12288], vec![fill; 900]);
    write(&root.join(dir).join("ext.img"), &image);
    write(&root.join(dir).join("ext.roothash.p7s"), &signature);
    let descriptor = serde_json::to_vec(&json!({
        "arch": arch,
        "content": {
            "image": {"bytes": image.len(), "sha256": hash(&image)},
            "rootHash": "22".repeat(32),
            "signature": {"bytes": signature.len(), "sha256": hash(&signature)},
            "verity": {"algorithm": "sha256", "dataBlockSize": 4096, "dataBlocks": 2,
                "hashBlockSize": 4096, "hashOffset": 8192, "salt": "44".repeat(32),
                "version": 1},
        },
        "kernel": kernel,
        "name": "rtl8125",
        "schema": "mica/driver-ext-descriptor/v1",
        "version": format!("1.{fill}"),
    }))
    .unwrap();
    write(&root.join(dir).join("ext.json"), &descriptor);
    descriptor
}
/// After an extension install, alone or with a system update, whatever was
/// interrupted: the confirmed build stays, a pending change names the new
/// build whole, is on its trial alone or paired with the update, and once the
/// install ran through it is there.
fn validate_extensions(root: &Path, kind: Kind, operation: &str, finished: bool) {
    if !operation.starts_with("ext-") {
        return;
    }
    let store = store(root, kind);
    let kernel = fs::read_to_string(root.join("ext-kernel")).unwrap();
    let next = fs::read_to_string(root.join("ext-next-kernel")).unwrap();
    let (old, new) = (
        fs::read_to_string(root.join("ext-old-id")).unwrap(),
        fs::read_to_string(root.join("ext-new-id")).unwrap(),
    );
    let state = mica_deploy::extensions::read_state(&store.system).unwrap();
    assert_eq!(state.builds[&kernel]["rtl8125"], old);
    held_build(root, kind, &store, &old);
    let Some(pending) = &state.pending else {
        assert!(!finished, "the change is pending");
        return;
    };
    assert_eq!(pending[&next]["rtl8125"], new);
    held_build(root, kind, &store, &new);
    if operation == "ext-pair" {
        let candidate = fs::read_to_string(root.join("candidate-id")).unwrap();
        assert_eq!(state.pair.as_deref(), Some(candidate.as_str()));
        assert_eq!(
            pending[&kernel]["rtl8125"], old,
            "the running kernel's stays"
        );
    } else {
        assert_eq!(state.pair, None);
        let trial = mica_deploy::set_state::read_trial(&store.meta).unwrap();
        assert_eq!(trial.kind, mica_deploy::set_state::TrialKind::Extensions);
        assert_eq!(Some(trial.id), state.pending_id());
    }
    if finished && kind == Kind::Ubi {
        let names = ubi::DirUbi::open_at(&root.join("store/ubi")).names();
        assert!(!names.iter().any(|name| name.starts_with("p.")));
    }
}

/// The build `named` is held whole, as its descriptor says.
fn held_build(root: &Path, kind: Kind, store: &DeploymentStore, named: &str) {
    let directory = mica_deploy::extensions::object_dir(&store.system, named);
    let extension =
        mica_deploy::driver_ext::parse_descriptor(&fs::read(directory.join("ext.json")).unwrap())
            .unwrap();
    assert_eq!(extension.id, named);
    let content = &extension.descriptor.content;
    let image = match kind {
        Kind::Ubi => ubi::DirUbi::open_at(&root.join("store/ubi")).read(&format!("x-{named}")),
        _ => fs::read(directory.join("ext.img")).unwrap(),
    };
    for (bytes, artifact) in [
        (image, &content.image),
        (
            fs::read(directory.join("ext.roothash.p7s")).unwrap(),
            &content.signature,
        ),
    ] {
        assert_eq!(
            (bytes.len() as u64, hash(&bytes)),
            (artifact.bytes, artifact.sha256.clone())
        );
    }
}
/// A signed general core set of one component, `micad` at `version`, whose
/// content is `first`'s root's, and its id.
fn core_set(
    key: &Ed25519KeyPair,
    public: &[u8; 32],
    first: &Value,
    generation: u64,
    version: &str,
) -> (Vec<u8>, String) {
    let mut core = json!({
        "schema": "mica/core/v1", "arch": first["arch"], "package": "micad",
        "version": version, "features": ["micad"], "needs": [], "root": {"min": 1},
        "content": first["rootfs"]["content"],
    });
    core["id"] = json!(component_id(&core).unwrap());
    let set = serde_json::to_vec(&json!({
        "schema": "mica/set/v1", "kind": "core", "channel": "general", "arch": first["arch"],
        "generation": generation, "version": version, "components": [core],
    }))
    .unwrap();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hash(public),
        STANDARD.encode(&set),
        STANDARD.encode(key.sign(&set))
    );
    (envelope.into_bytes(), hash(&set))
}

fn run_operation(root: &Path, kind: Kind, operation: &str) {
    let store = store(root, kind);
    let public: [u8; 32] = fs::read(root.join("public")).unwrap().try_into().unwrap();
    let receipt =
        serde_json::from_slice(&fs::read(root.join("candidate-receipt.json")).unwrap()).unwrap();
    match operation {
        "install" => {
            store
                .install(
                    &fs::read(root.join("candidate.json")).unwrap(),
                    &[public],
                    &Target {
                        board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                        arch: if kind.fit() { "arm64" } else { "amd64" },
                        product: "uefi-x64-dev",
                        features: &[],
                    },
                    &root.join("objects"),
                    &serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                        .unwrap(),
                    None,
                )
                .unwrap();
        }
        "ext-pair" => {
            store
                .install_with_extensions(
                    &fs::read(root.join("candidate.json")).unwrap(),
                    &[public],
                    &Target {
                        board: if kind.fit() { "cx3576" } else { "uefi-x64" },
                        arch: if kind.fit() { "arm64" } else { "amd64" },
                        product: "uefi-x64-dev",
                        features: &[],
                    },
                    &root.join("objects"),
                    &serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                        .unwrap(),
                    None,
                    &mica_deploy::deployments::KernelExtensions {
                        descriptors: vec![fs::read(root.join("ext-2/ext.json")).unwrap()],
                        dropped: Vec::new(),
                    },
                )
                .unwrap();
        }
        "confirm" => {
            store.confirm(&receipt).unwrap();
        }
        "gc" => {
            store.collect(&receipt, &[public]).unwrap();
        }
        "ext-install" => {
            let running: BootReceipt =
                serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                    .unwrap();
            store
                .install_extension(
                    &fs::read(root.join("ext-2/ext.json")).unwrap(),
                    &root.join("ext-2/ext.img"),
                    &root.join("ext-2/ext.roothash.p7s"),
                    &[public],
                    &running.deployment_id,
                )
                .unwrap();
            store
                .collect_extensions(&std::collections::BTreeSet::new(), &[public])
                .unwrap();
        }
        "import" => {
            let workspace = root.join("store/updates");
            let store = store.with_staged(workspace.join("verified"));
            let keys = [public];
            mica_deploy::acquisition::Acquisition {
                root: workspace,
                store: &store,
                keys: &keys,
                board: "cx3576",
                arch: "arm64",
                product: "uefi-x64-dev",
                max_bytes: Some(1 << 26),
                release: None,
            }
            .import_any(&mut fs::File::open(root.join("candidate.archive")).unwrap())
            .unwrap();
        }
        "reset-stage" => {
            // The running deployment, the one before the pending candidate.
            let running: BootReceipt =
                serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                    .unwrap();
            if mica_deploy::factory_reset::staged(&store.system)
                .unwrap()
                .is_none()
            {
                store.prepare_factory_reset(&running).unwrap();
                mica_deploy::factory_reset::stage(
                    &store.system,
                    &store.meta,
                    &reset_request(&running.deployment_id),
                )
                .unwrap();
            }
        }
        "reset-finish" => {
            if let Some(request) = mica_deploy::factory_reset::staged(&store.system).unwrap() {
                store.finish_factory_reset(&receipt, &[public]).unwrap();
                mica_deploy::factory_reset::record(
                    &store.meta,
                    &mica_deploy::factory_reset::Done {
                        v: 2,
                        requested: request.requested,
                        by: request.by,
                        source: request.source,
                        completed: request.requested + 60,
                        unremoved: 0,
                        formatted: Some(true),
                    },
                )
                .unwrap();
                mica_deploy::factory_reset::unstage(&store.system).unwrap();
            } else {
                // Interrupted once the request was gone: the rest goes, as
                // mica-deploy's next command removes it.
                mica_deploy::factory_reset::unstage(&store.system).unwrap();
            }
        }
        _ => panic!("unknown operation"),
    }
}
/// A factory reset of version 2 back to deployment `id`.
fn reset_request(id: &str) -> mica_deploy::factory_reset::Request {
    mica_deploy::factory_reset::Request {
        v: 2,
        requested: 1_700_000_000,
        by: mica_deploy::factory_reset::Requester::Cli,
        source: "io faults".into(),
        deployment: Some(id.into()),
    }
}
/// After a factory reset is staged or finished, whatever was interrupted: a
/// staged request always has its whole snapshot, and a finished one left the
/// running deployment and its core set alone.
fn validate_reset(root: &Path, kind: Kind, operation: &str) {
    let store = store(root, kind);
    let snapshot = store.system.join("factory-reset/snapshot");
    match mica_deploy::factory_reset::staged(&store.system).unwrap() {
        Some(request) => {
            for name in ["catalog.json", "firmware.json", "deployments.json"] {
                assert_eq!(
                    fs::read(snapshot.join(name)).unwrap(),
                    fs::read(store.meta.join(name)).unwrap(),
                    "{name}"
                );
            }
            if operation == "reset-stage" {
                assert!(store.effective_state().unwrap().candidate.is_none());
                assert_eq!(
                    request.deployment.as_deref(),
                    Some(
                        fs::read_to_string(root.join("retained-id"))
                            .unwrap()
                            .as_str()
                    )
                );
            }
        }
        None if operation == "reset-finish" => {
            let running = fs::read_to_string(root.join("retained-id")).unwrap();
            let entries = store.entries().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, running);
            let state = store.effective_state().unwrap();
            assert_eq!(state.fallback, None);
            assert_eq!(state.current.as_deref(), Some(running.as_str()));
            assert!(!store.system.join("factory-reset").exists());
            assert_eq!(
                mica_deploy::factory_reset::done(&store.meta)
                    .unwrap()
                    .unwrap()
                    .formatted,
                Some(true)
            );
        }
        None => {}
    }
}
/// After an import on UBI, run again past any interruption: the candidate's
/// objects are whole in their volumes, staged for installation, and nothing
/// pending is left.
fn validate_import(root: &Path) {
    let store = store(root, Kind::Ubi);
    let public: [u8; 32] = fs::read(root.join("public")).unwrap().try_into().unwrap();
    let candidate =
        authenticate_deployment(&fs::read(root.join("candidate.json")).unwrap(), &[public])
            .unwrap();
    let device = ubi::DirUbi::open_at(&root.join("store/ubi"));
    for (place, artifact) in store.object_paths(&candidate) {
        if let mica_deploy::deployments::Place::Volume(name) = place {
            mica_deploy::ubi::verify_object(&device, &name, artifact).unwrap();
        }
    }
    assert!(device.names().iter().all(|name| !name.starts_with("p.")));
    let id = component_id(&serde_json::to_value(&candidate).unwrap()).unwrap();
    assert!(
        root.join(format!("store/updates/verified/{id}.json"))
            .is_file()
    );
}
fn validate(root: &Path, kind: Kind) {
    let store = store(root, kind);
    let public: [u8; 32] = fs::read(root.join("public")).unwrap().try_into().unwrap();
    let mut entries = store.entries().unwrap();
    assert!(entries.len() <= 2, "a third deployment was published");
    let retained = fs::read_to_string(root.join("retained-id")).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.id == retained && e.tries_left != Some(0))
    );
    if let BootBackend::Ubi { .. } = &store.boot {
        let device = ubi::DirUbi::open_at(&root.join("store/ubi"));
        for name in mica_deploy::ubi::RECORDS {
            // Either redundant copy alone, as a reader that loses the other
            // would see it.
            let isolated = tempfile::TempDir::new().unwrap();
            let alone = ubi::DirUbi::with_fixed_volumes(isolated.path(), 1024);
            let volume = mica_deploy::ubi::find(&alone, name).unwrap().unwrap();
            fs::write(
                mica_deploy::ubi::UbiDevice::path(&alone, volume.id),
                device.read(name),
            )
            .unwrap();
            if let Ok(environment) = mica_deploy::fit_env::Environment::load_volumes(&alone) {
                for record in environment.records {
                    if !entries.iter().any(|entry| entry.id == record.id) {
                        entries.push(mica_deploy::deployments::Entry {
                            id: record.id,
                            file: String::new(),
                            generation: record.generation,
                            tries_left: record.tries_left,
                        });
                    }
                }
            }
        }
    }
    if let BootBackend::Fit { firmware, .. } = &store.boot {
        let bytes = fs::read(firmware).unwrap();
        for offset in boards::layout("cx3576").offsets() {
            // Either CRC-valid redundant copy must refer only to complete
            // deployments, even if a later read loses the newest copy.
            let mut isolated = tempfile::NamedTempFile::new().unwrap();
            isolated.as_file().set_len(18 * 1048576 - 32768).unwrap();
            isolated.seek(SeekFrom::Start(offset)).unwrap();
            isolated
                .write_all(
                    &bytes[offset as usize..offset as usize + mica_deploy::fit_env::ENV_SIZE],
                )
                .unwrap();
            if let Ok(environment) =
                mica_deploy::fit_env::Environment::load(isolated.path(), boards::layout("cx3576"))
            {
                let records = environment.records;
                for record in records {
                    if !entries.iter().any(|entry| entry.id == record.id) {
                        entries.push(mica_deploy::deployments::Entry {
                            id: record.id,
                            file: String::new(),
                            generation: record.generation,
                            tries_left: record.tries_left,
                        });
                    }
                }
            }
        }
    }
    for entry in &entries {
        let d = authenticate_deployment(
            &fs::read(store.system.join(format!("deployments/{}.json", entry.id))).unwrap(),
            &[public],
        )
        .unwrap();
        let value = serde_json::to_value(&d).unwrap();
        assert_eq!(component_id(&value).unwrap(), entry.id);
        // Every object the deployment lists.
        for (path, artifact) in store.object_paths(&d) {
            let bytes = fs::read(store.readable(&path).unwrap().unwrap()).unwrap();
            assert_eq!(bytes.len() as u64, artifact.bytes);
            assert_eq!(hash(&bytes), artifact.sha256);
        }
    }
    store.effective_state().unwrap();
}
