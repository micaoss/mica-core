//! Acquiring a core set: the catalog's core line, and a core archive.

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    acquisition::Imported,
    components::component_id,
    core_state::{self, Composition},
    deployments::CoreTarget,
};
use serde_json::{Value, json};
use std::{fs, io::Cursor};

use super::*;

fn sha(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

/// A signed set of one component, `micad`, whose image is `image` and whose
/// signature object is `signature`; the trusted key; and the set's id.
fn core_set(generation: u64, image: &[u8], signature: &[u8]) -> (String, [u8; 32], String) {
    let mut core = json!({
        "schema": "mica/core/v1", "arch": "amd64", "package": "micad", "version": "0.0.6",
        "features": ["micad"], "needs": [], "root": {"min": 1},
        "content": {
            "image": {"bytes": image.len(), "sha256": sha(image)},
            "rootHash": "ab".repeat(32),
            "signature": {"bytes": signature.len(), "sha256": sha(signature)},
            "verity": {"version": 1, "algorithm": "sha256", "dataBlockSize": 4096,
                "hashBlockSize": 4096, "dataBlocks": 2, "hashOffset": 8192, "salt": "cd".repeat(32)},
        },
    });
    core["id"] = json!(component_id(&core).unwrap());
    let payload = serde_json::to_vec(&json!({
        "schema": "mica/core-set/v1", "channel": "general", "arch": "amd64",
        "generation": generation, "version": "0.0.6", "components": [core],
    }))
    .unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha(&public),
        STANDARD.encode(&payload),
        STANDARD.encode(key.sign(&payload).as_ref())
    );
    (envelope, public, sha(&payload))
}

/// A MICAUPD1 archive of `envelope` and `objects`, sorted by digest.
fn core_archive(envelope: &str, objects: &[&[u8]]) -> Vec<u8> {
    let mut sorted: Vec<&[u8]> = objects.to_vec();
    sorted.sort_by_key(|bytes| sha(bytes));
    let mut archive = b"MICAUPD1".to_vec();
    archive.extend((envelope.len() as u32).to_be_bytes());
    archive.extend(envelope.as_bytes());
    archive.extend((sorted.len() as u32).to_be_bytes());
    for bytes in sorted {
        archive.extend(sha(bytes).as_bytes());
        archive.extend((bytes.len() as u64).to_be_bytes());
        archive.extend(bytes);
    }
    archive
}

fn target(features: &[String]) -> CoreTarget<'_> {
    CoreTarget {
        arch: "amd64",
        features,
        root_level: 1,
    }
}

/// The core line end to end: the manifest, the core release's document, the
/// signed set, then each object, and what is ready installs and is what the
/// runkit composes.
#[test]
pub(super) fn a_core_set_is_checked_fetched_installed_and_composed() {
    let image = vec![42_u8; 12288];
    let signature = vec![7_u8; 64];
    let (envelope, key, id) = core_set(1, &image, &signature);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let manifest = serde_json::to_string(&json!({
        "schema": "mica/catalog/v2", "revision": 1, "baseUrl": format!("{origin}/"),
        "products": [],
        "cores": [{"channel": "general", "arch": "amd64",
            "latest": {"id": "core.general.20261010-0000", "generation": 1,
                "path": "mica/core.general/20261010-0000/amd64/index.json"}}],
    }))
    .unwrap();
    let mut listed: Vec<(String, usize)> = vec![
        (sha(&image), image.len()),
        (sha(&signature), signature.len()),
    ];
    listed.sort();
    let release = serde_json::to_string(&json!({
        "schema": "mica/core-release/v1",
        "baseUrl": format!("{origin}/mica/core.general/20261010-0000/amd64/"),
        "id": "core.general.20261010-0000", "channel": "general", "arch": "amd64",
        "generation": 1, "version": "0.0.6",
        "coreSet": {"path": "core-set.json", "sha256": sha(envelope.as_bytes()), "bytes": envelope.len()},
        "objects": listed.iter().map(|(sha, bytes)|
            json!({"sha256": sha, "bytes": bytes, "path": format!("objects/{sha}")})).collect::<Vec<_>>(),
    }))
    .unwrap();
    // The first object is probed for a transfer index, which this origin does
    // not publish; every object after that is asked for whole.
    let by_sha = |wanted: &str| -> Vec<u8> {
        if wanted == sha(&image) {
            image.clone()
        } else {
            signature.clone()
        }
    };
    let server = serve(
        listener,
        vec![
            (ok(manifest.as_bytes()), NO_HOLD),
            (ok(release.as_bytes()), NO_HOLD),
            (ok(envelope.as_bytes()), NO_HOLD),
            (delta::http(404, b""), NO_HOLD),
            (ok(&by_sha(&listed[0].0)), NO_HOLD),
            (ok(&by_sha(&listed[1].0)), NO_HOLD),
        ],
    );
    let (dir, store) = fixture();
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let source = format!("{origin}/update/");

    let selected = acq
        .core_check(&source, "general")
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(selected.core_set_id, id);
    let ready = acq.core_fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /update/v2/manifest.json "));
    assert!(requests[1].starts_with("GET /mica/core.general/20261010-0000/amd64/index.json "));
    assert!(requests[2].starts_with("GET /mica/core.general/20261010-0000/amd64/core-set.json "));
    assert!(
        requests[4].starts_with(&format!(
            "GET /mica/core.general/20261010-0000/amd64/objects/{} ",
            listed[0].0
        )),
        "{}",
        requests[4]
    );
    assert_eq!((ready.id.as_str(), ready.generation), (id.as_str(), 1));
    assert_eq!(fs::read(&ready.path).unwrap(), envelope.as_bytes());

    let features = vec!["micad".to_string()];
    store
        .install_core(
            &fs::read(&ready.path).unwrap(),
            &keys,
            &target(&features),
            &ready.objects,
        )
        .unwrap();
    match core_state::composition(
        &store.system,
        &store.meta,
        &keys,
        "amd64",
        &features,
        1,
        |_| {},
    )
    .unwrap()
    {
        Composition::Set {
            id: composed,
            pending,
            ..
        } => {
            assert_eq!((composed, pending), (id, true));
        }
        Composition::Deployment => panic!("the installed set was not composed"),
    }
}

/// A core archive is the same container with the signed set as its envelope,
/// and an import says which of the two an archive carried.
#[test]
pub(super) fn an_archive_is_imported_as_what_its_envelope_is() {
    let image = vec![42_u8; 12288];
    let signature = vec![7_u8; 64];
    let (envelope, key, id) = core_set(1, &image, &signature);
    let (dir, store) = fixture();
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);

    let archive = core_archive(&envelope, &[&image, &signature]);
    match acq.import_any(&mut Cursor::new(&archive)).unwrap() {
        Imported::CoreSet(ready) => {
            assert_eq!(ready.id, id);
            assert_eq!(ready.channel, "general");
            assert_eq!(fs::read(acq.objects().join(sha(&image))).unwrap(), image);
        }
        Imported::Deployment(_) => panic!("a core archive was read as a deployment"),
    }
    let value = serde_json::to_value(acq.import_any(&mut Cursor::new(&archive)).unwrap()).unwrap();
    assert_eq!(value["kind"], "coreSet");

    // The deployment archive of the other fixtures, through the same door.
    let (deployment, _, _) = archive_fixture();
    let value: Value =
        serde_json::to_value(acq.import_any(&mut Cursor::new(&deployment)).unwrap()).unwrap();
    assert_eq!(value["kind"], "deployment");
    assert!(value["id"].is_string());
}

/// What a core archive is refused for.
#[test]
pub(super) fn a_core_archive_that_is_incomplete_foreign_or_altered_is_refused() {
    let image = vec![42_u8; 12288];
    let signature = vec![7_u8; 64];
    let (envelope, key, _) = core_set(1, &image, &signature);
    let fresh = || {
        let (dir, store) = fixture();
        (dir, store)
    };

    // An object the set does not name.
    let (dir, store) = fresh();
    let keys = [key];
    let extra = vec![9_u8; 16];
    let archive = core_archive(&envelope, &[&image, &signature, &extra]);
    assert!(
        acquisition(&dir, &store, &keys)
            .import_any(&mut Cursor::new(archive))
            .is_err()
    );

    // An object missing, and nothing on the device to stand in for it.
    let (dir, store) = fresh();
    let archive = core_archive(&envelope, &[&image]);
    let error = acquisition(&dir, &store, &keys)
        .import_any(&mut Cursor::new(archive))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("core set objects are incomplete"),
        "{error:#}"
    );

    // An untrusted key, and a set for another architecture.
    let (dir, store) = fresh();
    let archive = core_archive(&envelope, &[&image, &signature]);
    let other = [[0_u8; 32]];
    assert!(
        acquisition(&dir, &store, &other)
            .import_any(&mut Cursor::new(&archive))
            .is_err()
    );
    let arm = Acquisition {
        arch: "arm64",
        ..acquisition(&dir, &store, &keys)
    };
    assert!(arm.import_any(&mut Cursor::new(&archive)).is_err());

    // Altered bytes under a signed digest.
    let (dir, store) = fresh();
    let mut altered = core_archive(&envelope, &[&image, &signature]);
    let last = altered.len() - 1;
    altered[last] ^= 1;
    assert!(
        acquisition(&dir, &store, &keys)
            .import_any(&mut Cursor::new(altered))
            .is_err()
    );
}
