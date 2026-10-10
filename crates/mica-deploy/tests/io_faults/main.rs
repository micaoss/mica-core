//! Process-interruption and ENOSPC acceptance for the real transaction implementation.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../support/boards.rs"]
mod boards;

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

fn store(root: &Path, fit: bool) -> DeploymentStore {
    DeploymentStore::new(
        root.join("store/system"),
        if fit {
            BootBackend::Fit {
                layout: boards::layout("cx3576"),
                firmware: root.join("store/firmware.img"),
            }
        } else {
            BootBackend::Uefi {
                esp: root.join("store/esp"),
            }
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
fn receipt(d: &Value, fit: bool) -> BootReceipt {
    let id = component_id(d).unwrap();
    BootReceipt {
        backend: if fit {
            BootKind::UbootFit
        } else {
            BootKind::Uefi
        },
        entry: if fit {
            format!("fit:{id}")
        } else {
            format!("mica-{id}.conf")
        },
        deployment_id: id,
        kernel_id: d["kernel"]["id"].as_str().unwrap().into(),
        rootfs_id: d["rootfs"]["id"].as_str().unwrap().into(),
        content_verified: true,
        boot_verified: true,
        secure_boot: !fit,
    }
}
fn paths(root: &Path, d: &Value, fit: bool) -> Vec<PathBuf> {
    let kernel = d["kernel"]["id"].as_str().unwrap();
    let rootfs = d["rootfs"]["id"].as_str().unwrap();
    vec![
        if fit {
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
fn seed(root: &Path, fit: bool, operation: &str) {
    let store = store(root, fit);
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
        if fit {
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
        d["generation"] = json!(generation);
        // The candidate carries a core component, so installation,
        // confirmation and collection move its objects too.
        if generation == 3 {
            // Its own bytes, so no object of it is also an object of the root.
            let core_payload = vec![77_u8; 12288];
            write(
                &root.join("objects").join(hash(&core_payload)),
                &core_payload,
            );
            let core_artifact = json!({"bytes": core_payload.len(), "sha256": hash(&core_payload)});
            let mut content = d["rootfs"]["content"].clone();
            content["image"] = core_artifact.clone();
            content["signature"] = core_artifact;
            let mut core = json!({
                "schema": "mica/core/v1", "id": "", "arch": d["arch"], "package": "micad",
                "version": "1.0.0", "features": ["micad"], "needs": [], "root": {"min": 1},
                "content": content,
            });
            core["id"] = json!(component_id(&core).unwrap());
            d["core"] = json!([core]);
        }
        d["kernel"]["id"] = json!(component_id(&d["kernel"]).unwrap());
        d["rootfs"]["id"] = json!(component_id(&d["rootfs"]).unwrap());
        let bytes = serde_json::to_vec(&d).unwrap();
        let envelope = format!(
            "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
            hash(&public), STANDARD.encode(&bytes), STANDARD.encode(key.sign(&bytes))
        ).into_bytes();
        write(&root.join("objects").join(hash(&payload)), &payload);
        if generation < 3 {
            for path in paths(root, &d, fit) {
                write(&path, &payload);
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
    if fit {
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
        &serde_json::to_vec(&receipt(&deployments[2], fit)).unwrap(),
    );
    write(&root.join("retained-id"), records[0].id.as_bytes());
    write(&root.join("retired-id"), records[1].id.as_bytes());
    write(
        &root.join("current-receipt.json"),
        &serde_json::to_vec(&receipt(&deployments[1], fit)).unwrap(),
    );
    if !["install", "reuse"].contains(&operation) {
        store
            .install(
                &envelopes[2],
                &[public],
                &Target {
                    board: if fit { "cx3576" } else { "uefi-x64" },
                    arch: if fit { "arm64" } else { "amd64" },
                    product: "uefi-x64-dev",
                    features: &[],
                },
                &root.join("objects"),
                &serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                    .unwrap(),
            )
            .unwrap();
    }
    if !["install", "reuse"].contains(&operation) {
        if let BootBackend::Fit { firmware, .. } = &store.boot {
            let mut environment =
                mica_deploy::fit_env::Environment::load(firmware, boards::layout("cx3576"))
                    .unwrap();
            environment.records[0].tries_left = Some(2);
            environment.save(firmware).unwrap();
        } else {
            let id = component_id(&deployments[2]).unwrap();
            fs::rename(
                root.join(format!("store/esp/loader/entries/mica-{id}+3.conf")),
                root.join(format!("store/esp/loader/entries/mica-{id}+2-1.conf")),
            )
            .unwrap();
        }
    }
    if operation == "gc" {
        store.confirm(&receipt(&deployments[2], fit)).unwrap();
        write(
            &store
                .system
                .join(format!("roots/{}/rootfs.img", "f".repeat(64))),
            b"unreachable",
        );
        write(
            &store
                .system
                .join(format!("cores/{}/core.img", "e".repeat(64))),
            b"unreachable",
        );
    }
}
fn run_operation(root: &Path, fit: bool, operation: &str) {
    let store = store(root, fit);
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
                        board: if fit { "cx3576" } else { "uefi-x64" },
                        arch: if fit { "arm64" } else { "amd64" },
                        product: "uefi-x64-dev",
                        features: &[],
                    },
                    &root.join("objects"),
                    &serde_json::from_slice(&fs::read(root.join("current-receipt.json")).unwrap())
                        .unwrap(),
                )
                .unwrap();
        }
        "confirm" => {
            store.confirm(&receipt).unwrap();
        }
        "gc" => {
            store.collect(&receipt, &[public]).unwrap();
        }
        _ => panic!("unknown operation"),
    }
}
fn validate(root: &Path, fit: bool) {
    let store = store(root, fit);
    let public: [u8; 32] = fs::read(root.join("public")).unwrap().try_into().unwrap();
    let mut entries = store.entries().unwrap();
    assert!(entries.len() <= 2, "a third deployment was published");
    let retained = fs::read_to_string(root.join("retained-id")).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.id == retained && e.tries_left != Some(0))
    );
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
        // Every object the deployment lists, core components included.
        for (path, artifact) in store.object_paths(&d) {
            let bytes = fs::read(&path).unwrap();
            assert_eq!(bytes.len() as u64, artifact.bytes);
            assert_eq!(hash(&bytes), artifact.sha256);
        }
    }
    store.effective_state().unwrap();
}
