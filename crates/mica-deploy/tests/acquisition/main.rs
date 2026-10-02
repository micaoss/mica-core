// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    acquisition::Acquisition, components::component_id, deployments::DeploymentStore,
};
use serde_json::{Value, json};
use std::{fs, io::Cursor};
use tempfile::TempDir;

mod delta;
mod online;
mod transfer;
use online::*;

fn archive() -> (Vec<u8>, [u8; 32], String) {
    let bytes = vec![42_u8; 12288];
    let sha = hex::encode(digest::digest(&digest::SHA256, &bytes));
    let mut d: Value =
        serde_json::from_str(include_str!("../component-contracts/deployment.json")).unwrap();
    for pointer in [
        "/kernel/boot/artifact",
        "/kernel/support/image",
        "/kernel/support/signature",
        "/rootfs/content/image",
        "/rootfs/content/signature",
    ] {
        *d.pointer_mut(pointer).unwrap() = json!({"bytes":bytes.len(),"sha256":sha});
    }
    d["kernel"]["id"] = component_id(&d["kernel"]).unwrap().into();
    d["rootfs"]["id"] = component_id(&d["rootfs"]).unwrap().into();
    let key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let payload = serde_json::to_vec(&d).unwrap();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hex::encode(digest::digest(&digest::SHA256, &public)),
        STANDARD.encode(&payload),
        STANDARD.encode(key.sign(&payload).as_ref())
    );
    let mut archive = b"MICAUPD1".to_vec();
    archive.extend((envelope.len() as u32).to_be_bytes());
    archive.extend(envelope.as_bytes());
    archive.extend(1_u32.to_be_bytes());
    archive.extend(sha.as_bytes());
    archive.extend((bytes.len() as u64).to_be_bytes());
    archive.extend(bytes);
    (archive, public, sha)
}
fn fixture() -> (TempDir, DeploymentStore) {
    let dir = TempDir::new().unwrap();
    for name in ["system", "esp", "meta"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    fs::create_dir_all(dir.path().join("esp/loader/entries")).unwrap();
    let store = DeploymentStore::new(
        dir.path().join("system"),
        mica_deploy::deployments::BootBackend::Uefi {
            esp: dir.path().join("esp"),
        },
        dir.path().join("meta"),
    );
    (dir, store)
}

#[test]
fn offline_archive_authenticates_before_staging_and_never_publishes_partial_descriptors() {
    let (archive, key, sha) = archive();
    let (dir, store) = fixture();
    let keys = [key];
    let acq = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(1024 * 1024),
    };
    let ready = acq.import(&mut Cursor::new(&archive)).unwrap();
    assert!(ready.path.is_file());
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), vec![42; 12288]);
    assert!(store.state().unwrap().candidate.is_none());
    assert!(acq.import(&mut Cursor::new(&archive)).is_ok());
    let mut wrong = archive.clone();
    wrong[8..12].copy_from_slice(&24577_u32.to_be_bytes());
    assert!(acq.import(&mut Cursor::new(wrong)).is_err());
    let mut extra = archive;
    extra.push(0);
    assert!(acq.import(&mut Cursor::new(extra)).is_err());
}

/// The archive magic is exactly the eight bytes `MICAUPD1`. Any other spelling is
/// refused even in front of an otherwise valid, correctly signed archive.
#[test]
fn only_the_micaupd1_magic_is_an_archive() {
    let (archive, key, _) = archive();
    assert_eq!(&archive[..8], b"MICAUPD1");
    let (dir, store) = fixture();
    let keys = [key];
    let acq = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(1024 * 1024),
    };
    for magic in [&b"MICAUPD2"[..], b"micaupd1", b"MICAUP01"] {
        let mut old = archive.clone();
        old[..8].copy_from_slice(magic);
        let error = acq.import(&mut Cursor::new(old)).unwrap_err();
        assert!(
            error.to_string().contains("invalid component archive"),
            "{}: {error:#}",
            String::from_utf8_lossy(magic)
        );
    }
    assert!(acq.import(&mut Cursor::new(&archive)).is_ok());
}

#[test]
fn interrupted_corrupt_oversized_and_foreign_archives_cannot_become_ready() {
    let (archive, key, _) = archive();
    for case in ["truncated", "digest", "budget", "board", "key", "symlink"] {
        let (dir, store) = fixture();
        let keys = [if case == "key" { [0; 32] } else { key }];
        let acq = Acquisition {
            root: dir.path().join("updates"),
            store: &store,
            keys: &keys,
            board: if case == "board" {
                "cx3576"
            } else {
                "uefi-x64"
            },
            arch: "amd64",
            product: "uefi-x64-dev",
            max_bytes: Some(if case == "budget" { 100 } else { 1024 * 1024 }),
        };
        let mut input = archive.clone();
        if case == "truncated" {
            input.truncate(input.len() - 1);
        }
        if case == "digest" {
            *input.last_mut().unwrap() = 0;
        }
        if case == "symlink" {
            std::os::unix::fs::symlink(dir.path().join("meta"), &acq.root).unwrap();
        }
        assert!(acq.import(&mut Cursor::new(input)).is_err(), "{case}");
        let verified = acq.root.join("verified");
        if verified.is_dir() {
            assert!(!fs::read_dir(verified).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            }));
        }
    }
}

#[test]
fn one_existing_destination_does_not_hide_other_destinations_for_the_same_digest() {
    let (archive, key, sha) = archive();
    let (dir, store) = fixture();
    let keys = [key];
    let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let deployment =
        mica_deploy::components::authenticate_deployment(&archive[12..12 + length], &keys).unwrap();
    let path = store.object_paths(&deployment)[0].0.clone();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![42; 12288]).unwrap();
    let acq = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(1024 * 1024),
    };
    acq.import(&mut Cursor::new(archive)).unwrap();
    assert!(acq.objects().join(sha).is_file());
}

#[test]
fn discard_is_bounded_to_acquisition_files_and_checks_all_paths_before_removing_any() {
    let (archive, key, _) = archive();
    let (dir, store) = fixture();
    let keys = [key];
    let acq = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(1024 * 1024),
    };
    let ready = acq.import(&mut Cursor::new(&archive)).unwrap();
    fs::write(store.meta.join("preserved"), b"metadata").unwrap();
    std::os::unix::fs::symlink(&store.meta, acq.root.join("downloads/link")).unwrap();
    assert!(acq.discard().is_err());
    assert!(ready.path.exists());
    fs::remove_file(acq.root.join("downloads/link")).unwrap();
    assert_eq!(acq.discard().unwrap(), 2);
    assert!(!ready.path.exists());
    assert_eq!(fs::read(store.meta.join("preserved")).unwrap(), b"metadata");
    assert_eq!(acq.discard().unwrap(), 0);
    assert!(acq.probe().is_ok());
    assert!(acq.import(&mut Cursor::new(&archive)).is_ok());
}

/// Discarding spends no budget, so it needs none; everything that writes to
/// the workspace is refused without the caller's budget.
#[test]
fn only_the_commands_that_write_need_a_workspace_budget() {
    let (archive, key, _) = archive();
    let (dir, store) = fixture();
    let keys = [key];
    let budgeted = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(1024 * 1024),
    };
    budgeted.import(&mut Cursor::new(&archive)).unwrap();
    let unbudgeted = Acquisition {
        max_bytes: None,
        ..budgeted
    };
    for error in [
        unbudgeted.probe().unwrap_err(),
        unbudgeted.import(&mut Cursor::new(&archive)).unwrap_err(),
    ] {
        assert!(
            format!("{error:#}").contains("no workspace budget"),
            "{error:#}"
        );
    }
    assert_eq!(unbudgeted.discard().unwrap(), 2);
}
