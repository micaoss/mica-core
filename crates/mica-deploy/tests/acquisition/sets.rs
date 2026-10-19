//! Acquiring a core set: the catalog's core line, and a core archive.

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    acquisition::Imported, components::component_id, core_set, deployments::SetTarget,
    update_set::SetKind,
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
    requiring(generation, image, signature, &Value::Null)
}

/// As [`core_set`], with `micad` requiring the root packages `requires`.
fn requiring(
    generation: u64,
    image: &[u8],
    signature: &[u8],
    requires: &Value,
) -> (String, [u8; 32], String) {
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
    if !requires.is_null() {
        core["requires"] = requires.clone();
    }
    core["id"] = json!(component_id(&core).unwrap());
    let payload = serde_json::to_vec(&json!({
        "schema": "mica/set/v1", "kind": "core", "channel": "general", "arch": "amd64",
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

fn target(features: &[String]) -> SetTarget<'_> {
    SetTarget {
        arch: "amd64",
        features,
        root: mica_deploy::components::RootFacts {
            level: 1,
            provides: None,
        },
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
        "schema": "mica/catalog/v1", "revision": 1, "baseUrl": format!("{origin}/"),
        "products": [],
        "sets": [{"kind": "core", "channel": "general", "arch": "amd64",
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
        "schema": "mica/set-release/v1",
        "baseUrl": format!("{origin}/mica/core.general/20261010-0000/amd64/"),
        "id": "core.general.20261010-0000", "kind": "core", "channel": "general",
        "arch": "amd64",
        "generation": 1, "version": "0.0.6",
        "set": {"path": "set.json", "sha256": sha(envelope.as_bytes()), "bytes": envelope.len()},
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

    let features = vec!["micad".to_string()];
    let selected = acq
        .set_check(SetKind::Core, &source, "general", &target(&features))
        .unwrap()
        .selected
        .unwrap();
    assert_eq!(selected.set_id, id);
    let ready = acq.set_fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /update/v1/manifest.json "));
    assert!(requests[1].starts_with("GET /mica/core.general/20261010-0000/amd64/index.json "));
    assert!(requests[2].starts_with("GET /mica/core.general/20261010-0000/amd64/set.json "));
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

    store
        .install_set(
            &fs::read(&ready.path).unwrap(),
            &keys,
            &target(&features),
            &ready.objects,
        )
        .unwrap();
    let composed = core_set::composition(
        &store.system,
        &store.meta,
        None,
        &keys,
        &core_set::BootRoot {
            arch: "amd64",
            features: &features,
            root: mica_deploy::components::RootFacts {
                level: 1,
                provides: None,
            },
        },
        &"a".repeat(64),
        |_| {},
    )
    .unwrap();
    assert_eq!((composed.id, composed.pending), (id, true));
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
        Imported::Set(ready) => {
            assert_eq!(ready.id, id);
            assert_eq!(ready.channel, "general");
            assert_eq!(fs::read(acq.objects().join(sha(&image))).unwrap(), image);
        }
        Imported::Deployment(_) => panic!("a core archive was read as a deployment"),
    }
    let value = serde_json::to_value(acq.import_any(&mut Cursor::new(&archive)).unwrap()).unwrap();
    assert_eq!(value["kind"], "set");

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
        format!("{error:#}").contains("set objects are incomplete"),
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

/// Make `envelope` the device's current core set.
fn hold(store: &mica_deploy::deployments::DeploymentStore, envelope: &str, id: &str) {
    let sets = store.system.join("sets/core");
    fs::create_dir_all(&sets).unwrap();
    fs::write(sets.join(format!("{id}.json")), envelope).unwrap();
    fs::write(sets.join("state.json"), format!("{{\"current\":\"{id}\"}}")).unwrap();
}

/// The reference a descriptor makes to its image's core set.
fn reference(envelope: &str, id: &str, generation: u64) -> Value {
    json!({"id": id, "channel": "general", "generation": generation,
        "sha256": sha(envelope.as_bytes()), "bytes": envelope.len()})
}

/// A newer set that does not select on the root it is checked for is not
/// offered: the check names it as waiting for a system update, and why.
#[test]
pub(super) fn a_set_that_does_not_select_waits_for_a_system_update() {
    let image = vec![42_u8; 12288];
    let signature = vec![7_u8; 64];
    let podman = json!([{"package": "mica-podman", "min": "6.1.4-7"}]);
    let (envelope, key, id) = requiring(1, &image, &signature, &podman);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let manifest = serde_json::to_string(&json!({
        "schema": "mica/catalog/v1", "revision": 1, "baseUrl": format!("{origin}/"),
        "products": [],
        "sets": [{"kind": "core", "channel": "general", "arch": "amd64",
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
        "schema": "mica/set-release/v1",
        "baseUrl": format!("{origin}/mica/core.general/20261010-0000/amd64/"),
        "id": "core.general.20261010-0000", "kind": "core", "channel": "general",
        "arch": "amd64", "generation": 1, "version": "0.0.6",
        "set": {"path": "set.json", "sha256": sha(envelope.as_bytes()), "bytes": envelope.len()},
        "objects": listed.iter().map(|(sha, bytes)|
            json!({"sha256": sha, "bytes": bytes, "path": format!("objects/{sha}")})).collect::<Vec<_>>(),
    }))
    .unwrap();
    let server = serve(
        listener,
        vec![
            (ok(manifest.as_bytes()), NO_HOLD),
            (ok(release.as_bytes()), NO_HOLD),
            (ok(envelope.as_bytes()), NO_HOLD),
        ],
    );
    let (dir, store) = fixture();
    let keys = [key];
    let features = vec!["micad".to_string()];
    let provides = [("mica-podman".to_owned(), "6.1.4-5".to_owned())].into();
    let catalog = acquisition(&dir, &store, &keys)
        .set_check(
            SetKind::Core,
            &format!("{origin}/update/"),
            "general",
            &SetTarget {
                root: mica_deploy::components::RootFacts {
                    level: 1,
                    provides: Some(&provides),
                },
                ..target(&features)
            },
        )
        .unwrap();
    server.join().unwrap();
    assert!(catalog.selected.is_none());
    let waiting = catalog.waits_for_system.unwrap();
    assert_eq!((waiting.id.as_str(), waiting.generation), (id.as_str(), 1));
    assert!(
        waiting.reason.contains("root requirement does not hold")
            && waiting.reason.contains("mica-podman >= 6.1.4-7"),
        "{}",
        waiting.reason
    );
}

/// An update archive whose deployment names a newer general set than the
/// device's carries that set with it; one that leaves part of it out, or
/// carries an object the set does not name, is refused.
#[test]
pub(super) fn an_update_archive_carries_the_image_set_a_system_update_takes() {
    let (held, key, held_id) = core_set(1, &[1_u8; 12288], &[1_u8; 64]);
    let image = vec![2_u8; 12288];
    let signature = vec![3_u8; 64];
    let (carried, _, carried_id) = core_set(2, &image, &signature);
    let named = Some(reference(&carried, &carried_id, 2));
    let keys = [key];

    let (archive, _, _) = archive_with(named.clone(), &[carried.as_bytes(), &image, &signature]);
    let (dir, store) = fixture();
    hold(&store, &held, &held_id);
    let acq = acquisition(&dir, &store, &keys);
    match acq.import_any(&mut Cursor::new(&archive)).unwrap() {
        Imported::Deployment(_) => {}
        Imported::Set(_) => panic!("a deployment archive was read as a set"),
    }
    for object in [carried.as_bytes(), &image, &signature] {
        assert!(acq.objects().join(sha(object)).is_file());
    }

    let (incomplete, _, _) = archive_with(named.clone(), &[carried.as_bytes(), &image]);
    let (dir, store) = fixture();
    hold(&store, &held, &held_id);
    let error = acquisition(&dir, &store, &keys)
        .import_any(&mut Cursor::new(&incomplete))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("set objects are incomplete"),
        "{error:#}"
    );

    let stray = vec![9_u8; 16];
    let (foreign, _, _) = archive_with(
        named.clone(),
        &[carried.as_bytes(), &image, &signature, &stray],
    );
    let (dir, store) = fixture();
    hold(&store, &held, &held_id);
    let acq = acquisition(&dir, &store, &keys);
    let error = acq.import_any(&mut Cursor::new(&foreign)).unwrap_err();
    assert!(
        format!("{error:#}").contains("objects its core set does not name"),
        "{error:#}"
    );
    assert!(!acq.objects().join(sha(&stray)).exists());

    // Without a set named, an extra object is what it always was.
    let (plain, _, _) = archive_with(None, &[&stray]);
    let (dir, store) = fixture();
    assert!(
        acquisition(&dir, &store, &keys)
            .import_any(&mut Cursor::new(&plain))
            .is_err()
    );
}

/// Online, a system update that takes the image's set fetches the set's
/// envelope and its components with the deployment.
#[test]
pub(super) fn a_system_update_fetches_the_image_set_it_takes() {
    let (held, key, held_id) = core_set(1, &[1_u8; 12288], &[1_u8; 64]);
    let image = vec![2_u8; 12288];
    let signature = vec![3_u8; 64];
    let (carried, _, carried_id) = core_set(2, &image, &signature);
    let (archive, _, sha_object) = archive_with(Some(reference(&carried, &carried_id, 2)), &[]);
    let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + length].to_vec()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let digest = sha(descriptor.as_bytes());
    let mut objects = [
        (sha_object.clone(), 12288_usize),
        (sha(carried.as_bytes()), carried.len()),
        (sha(&image), image.len()),
        (sha(&signature), signature.len()),
    ];
    objects.sort();
    let manifest = serde_json::to_string(&json!({"schema":"mica/catalog/v1","revision":1,
        "baseUrl":format!("{origin}/"),
        "products":[{"product":"uefi-x64-dev","board":"uefi-x64",
            "latest":{"id":"test","generation":1,"notes":"","path":"mica/uefi-x64-dev/1/index.json"}}]}))
    .unwrap();
    let release = serde_json::to_string(&json!({"schema":"mica/release/v1",
        "baseUrl":format!("{origin}/updates/"),
        "id":"test","product":"uefi-x64-dev","board":"uefi-x64","generation":1,"notes":"",
        "deployment":{"path":"deployment.json","sha256":digest,"bytes":descriptor.len()},
        "objects":objects.iter().map(|(sha, bytes)|
            json!({"sha256":sha,"bytes":bytes,"path":format!("objects/{sha}")})).collect::<Vec<_>>()}))
    .unwrap();
    let mut set_objects = [
        (sha(&image), image.clone()),
        (sha(&signature), signature.clone()),
    ];
    set_objects.sort();
    // Each batch of objects probes the origin for a transfer index first.
    let server = serve(
        listener,
        vec![
            (ok(manifest.as_bytes()), NO_HOLD),
            (ok(release.as_bytes()), NO_HOLD),
            (ok(descriptor.as_bytes()), NO_HOLD),
            (delta::http(404, b""), NO_HOLD),
            (ok(&[42_u8; 12288]), NO_HOLD),
            (delta::http(404, b""), NO_HOLD),
            (ok(carried.as_bytes()), NO_HOLD),
            (delta::http(404, b""), NO_HOLD),
            (ok(&set_objects[0].1), NO_HOLD),
            (ok(&set_objects[1].1), NO_HOLD),
        ],
    );
    let (dir, store) = fixture();
    hold(&store, &held, &held_id);
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let selected = acq
        .check(&format!("{origin}/update/"))
        .unwrap()
        .selected
        .unwrap();
    acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 10, "{requests:?}");
    for object in [carried.as_bytes(), &image, &signature] {
        assert!(acq.objects().join(sha(object)).is_file());
    }
}
