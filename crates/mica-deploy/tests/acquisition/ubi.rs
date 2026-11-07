//! Acquisition on a `ubi` board: the objects the store keeps in static volumes
//! are written straight into them, never staged on DATA.

use mica_deploy::{
    acquisition::Imported,
    components::Deployment,
    deployments::{BootBackend, Place},
    ubi::{UbiDevice, find},
};
use std::sync::Arc;

use super::ubi_device::{DirUbi, LEB};
use super::*;

/// The deployment's objects: a root, a support image and a kernel that the
/// store keeps in volumes, and one signature both roots of trust share, which
/// it keeps as files. Distinct, so each lands where it belongs. The two
/// verity images are 72 and 48 data blocks and their one-block trees: 3 and 2
/// LEBs; the kernel 2.
fn objects() -> [Vec<u8>; 4] {
    objects_of(1)
}

/// As [`objects`], of bytes from `base`: another deployment's.
fn objects_of(base: u8) -> [Vec<u8>; 4] {
    [
        vec![base; 73 * 4096],
        vec![base + 1; 49 * 4096],
        vec![base + 2; 150_000],
        vec![base + 3; 512],
    ]
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

fn artifact(bytes: &[u8]) -> Value {
    json!({"bytes": bytes.len(), "sha256": sha(bytes)})
}

/// A signed cx3576 descriptor of [`objects`], generation 2, and its key.
fn descriptor() -> (String, [u8; 32], Deployment) {
    descriptor_of(2, 1)
}

/// A signed cx3576 descriptor of generation `generation` over
/// [`objects_of`]`(base)`, and its key.
fn descriptor_of(generation: u64, base: u8) -> (String, [u8; 32], Deployment) {
    let [root, support, boot, signature] = objects_of(base);
    let mut d: Value =
        serde_json::from_str(include_str!("../component-contracts/deployment.json")).unwrap();
    d["board"] = json!("cx3576");
    d["arch"] = json!("arm64");
    d["kernel"]["board"] = json!("cx3576");
    d["kernel"]["arch"] = json!("arm64");
    d["kernel"]["boot"]["format"] = json!("fit");
    d["rootfs"]["arch"] = json!("arm64");
    d["kernel"]["boot"]["artifact"] = artifact(&boot);
    d["kernel"]["support"]["image"] = artifact(&support);
    d["kernel"]["support"]["signature"] = artifact(&signature);
    d["rootfs"]["content"]["image"] = artifact(&root);
    d["rootfs"]["content"]["signature"] = artifact(&signature);
    for (pointer, blocks) in [
        ("/rootfs/content/verity", 72),
        ("/kernel/support/verity", 48),
    ] {
        let verity = d.pointer_mut(pointer).unwrap();
        verity["dataBlocks"] = json!(blocks);
        verity["hashOffset"] = json!(blocks * 4096);
    }
    d["generation"] = json!(generation);
    d["kernel"]["id"] = component_id(&d["kernel"]).unwrap().into();
    d["rootfs"]["id"] = component_id(&d["rootfs"]).unwrap().into();
    let key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let payload = serde_json::to_vec(&d).unwrap();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha(&public),
        STANDARD.encode(&payload),
        STANDARD.encode(key.sign(&payload).as_ref())
    );
    let deployment = serde_json::from_value(d).unwrap();
    (envelope, public, deployment)
}

/// MICAUPD1 of `envelope` and every object of [`objects`].
fn archive(envelope: &str) -> Vec<u8> {
    let mut archive = b"MICAUPD1".to_vec();
    archive.extend((envelope.len() as u32).to_be_bytes());
    archive.extend(envelope.as_bytes());
    archive.extend(4_u32.to_be_bytes());
    for object in objects() {
        archive.extend(sha(&object).as_bytes());
        archive.extend((object.len() as u64).to_be_bytes());
        archive.extend(&object);
    }
    archive
}

/// A `ubi` store of `lebs` erase blocks keeping 4 MiB for the system, and the
/// device it is on.
fn store(dir: &TempDir, lebs: u64) -> (DeploymentStore, DirUbi) {
    let device = DirUbi::with_fixed_volumes(&dir.path().join("ubi"), lebs);
    fs::create_dir_all(dir.path().join("system")).unwrap();
    fs::create_dir_all(dir.path().join("meta")).unwrap();
    let store = DeploymentStore::new(
        dir.path().join("system"),
        BootBackend::Ubi {
            ubi: Arc::new(DirUbi::open_at(&dir.path().join("ubi"))),
        },
        dir.path().join("meta"),
    )
    .with_reserves(serde_json::from_value(json!({"system": 4 << 20, "data": 1 << 20})).unwrap())
    .with_staged(dir.path().join("updates/verified"));
    // The deployment the device runs: its descriptor, objects and records.
    let (envelope, _, running) = descriptor_of(1, 11);
    let id = component_id(&serde_json::to_value(&running).unwrap()).unwrap();
    let bytes = objects_of(11);
    fs::create_dir_all(dir.path().join("system/deployments")).unwrap();
    fs::write(
        dir.path().join(format!("system/deployments/{id}.json")),
        &envelope,
    )
    .unwrap();
    for (place, artifact) in store.object_paths(&running) {
        let content = bytes.iter().find(|b| sha(b) == artifact.sha256).unwrap();
        match place {
            Place::File(path) => {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            Place::Volume(name) => {
                device.put_static(&name, content);
            }
        }
    }
    let record = mica_deploy::fit_env::Record {
        id: id.clone(),
        kernel_id: running.kernel.id.clone(),
        generation: 1,
        tries_left: None,
    };
    for (copy, name) in mica_deploy::ubi::RECORDS.into_iter().enumerate() {
        let volume = find(&device, name).unwrap().unwrap();
        fs::write(
            device.path(volume.id),
            mica_deploy::fit_env::encode(std::slice::from_ref(&record), copy as u8).unwrap(),
        )
        .unwrap();
    }
    store
        .save_state(&mica_deploy::deployments::State {
            current: Some(id),
            highest_generation: 1,
            ..Default::default()
        })
        .unwrap();
    (store, device)
}

fn acquisition<'a>(
    dir: &TempDir,
    store: &'a DeploymentStore,
    keys: &'a [[u8; 32]],
) -> Acquisition<'a> {
    Acquisition {
        root: dir.path().join("updates"),
        store,
        keys,
        board: "cx3576",
        arch: "arm64",
        product: "uefi-x64-dev",
        max_bytes: Some(4 << 20),
        release: None,
    }
}

/// The volumes the deployment's objects go to.
fn volumes(d: &Deployment) -> [String; 3] {
    [
        format!("r-{}", d.rootfs.id),
        format!("s-{}", d.kernel.id),
        format!("k-{}", d.kernel.id),
    ]
}

/// Every object the store keeps in a volume is there, of its signed bytes,
/// and no pending volume is left.
fn assert_in_volumes(device: &DirUbi, d: &Deployment) {
    let [root, support, boot, _] = objects();
    for (name, bytes) in volumes(d).into_iter().zip([root, support, boot]) {
        assert_eq!(device.read(&name), bytes, "{name}");
    }
    assert!(
        device.names().iter().all(|name| !name.starts_with("p.")),
        "{:?}",
        device.names()
    );
}

#[test]
fn an_import_writes_the_large_objects_straight_into_their_volumes() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let Imported::Deployment(ready) = acq
        .import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap()
    else {
        panic!("not a deployment");
    };
    assert_in_volumes(&device, &d);
    // Only the signature, a file of SYSTEM, is staged on DATA.
    let staged: Vec<_> = fs::read_dir(&ready.objects)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(staged, [sha(&objects()[3])]);
}

#[test]
fn staged_objects_are_held_until_the_workspace_is_discarded() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    acq.import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap();
    // Unreachable and interrupted volumes go; the staged deployment's stay.
    device.put_static(&format!("r-{}", "f".repeat(64)), b"unreachable");
    device.put_static(&format!("p.k-{}", "f".repeat(64)), b"interrupted");
    assert_eq!(store.release_volumes(&keys).unwrap(), 2);
    assert_in_volumes(&device, &d);
    let available = device.available_lebs().unwrap();
    // Discarding the workspace releases what the fetch wrote for it.
    assert_eq!(acq.discard().unwrap(), 2 + 3);
    for name in volumes(&d) {
        assert!(find(&device, &name).unwrap().is_none(), "{name}");
    }
    assert_eq!(device.available_lebs().unwrap(), available + 3 + 2 + 2);
}

#[test]
fn an_interrupted_volume_is_written_again() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let pending = format!("p.{}", volumes(&d)[0]);
    device.put_static(&pending, b"half of a root");
    device.interrupt_update(&pending);
    acquisition(&dir, &store, &keys)
        .import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap();
    assert_in_volumes(&device, &d);
}

#[test]
fn an_object_that_does_not_match_its_digest_leaves_no_volume() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let mut altered = archive(&envelope);
    let last = altered.len() - 600;
    altered[last] ^= 1;
    let available = device.available_lebs().unwrap();
    assert!(
        acquisition(&dir, &store, &keys)
            .import_any(&mut Cursor::new(altered))
            .is_err()
    );
    // The kernel, last of the large objects, is altered: the ones before it
    // landed, and nothing pending is left.
    assert!(device.names().iter().all(|name| !name.starts_with("p.")));
    assert!(find(&device, &volumes(&d)[0]).unwrap().is_some());
    assert!(find(&device, &volumes(&d)[2]).unwrap().is_none());
    assert_eq!(device.available_lebs().unwrap(), available - 3 - 2);
}

#[test]
fn objects_that_do_not_fit_are_refused_unless_the_fallback_is_released() {
    // Fixed volumes 130, the running deployment's 7, a filler of 2, and 40
    // free: the objects take 3 + 2 + 2 LEBs beside the 34 the system reserve
    // keeps.
    let filled = |dir: &TempDir| {
        let (store, device) = store(dir, 179);
        device.put_static("filler", &vec![0; 2 * LEB as usize]);
        (store, device)
    };
    let (envelope, key, d) = descriptor();
    let keys = [key];

    let dir = TempDir::new().unwrap();
    let (store, device) = filled(&dir);
    let error = acquisition(&dir, &store, &keys)
        .import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("insufficient UBI space"),
        "{error:#}"
    );
    assert!(find(&device, &volumes(&d)[0]).unwrap().is_none());

    let dir = TempDir::new().unwrap();
    let (store, device) = filled(&dir);
    let released = std::cell::Cell::new(None);
    let release = |incoming: &Deployment| {
        released.set(Some(incoming.rootfs.id.clone()));
        let filler = find(&device, "filler")?.unwrap();
        device.remove(filler.id)
    };
    let acq = Acquisition {
        release: Some(&release),
        ..acquisition(&dir, &store, &keys)
    };
    acq.import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap();
    assert_eq!(released.take(), Some(d.rootfs.id.clone()));
    assert_in_volumes(&device, &d);
}

#[test]
fn the_budget_counts_what_is_written_into_the_store() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let acq = Acquisition {
        max_bytes: Some(600_000),
        ..acquisition(&dir, &store, &keys)
    };
    let error = acq
        .import_any(&mut Cursor::new(archive(&envelope)))
        .unwrap_err();
    assert!(format!("{error:#}").contains("budget"), "{error:#}");
    assert!(find(&device, &volumes(&d)[0]).unwrap().is_none());
}

#[test]
fn a_fetch_writes_into_the_volumes_smallest_first() {
    let dir = TempDir::new().unwrap();
    let (store, device) = store(&dir, 600);
    let (envelope, key, d) = descriptor();
    let keys = [key];
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut listed: Vec<Vec<u8>> = objects().into();
    listed.sort_by_key(|bytes| (bytes.len(), sha(bytes)));
    let manifest = serde_json::to_string(&json!({"schema":"mica/catalog/v1","revision":1,
        "baseUrl":format!("{origin}/"),
        "products":[{"product":"uefi-x64-dev","board":"cx3576",
            "latest":{"id":"test","generation":2,"notes":"","path":"mica/uefi-x64-dev/1/index.json"}}]}))
    .unwrap();
    let release = serde_json::to_string(&json!({"schema":"mica/release/v1",
        "baseUrl":format!("{origin}/updates/"),
        "id":"test","product":"uefi-x64-dev","board":"cx3576","generation":2,"notes":"",
        "deployment":{"path":"deployment.json","sha256":sha(envelope.as_bytes()),"bytes":envelope.len()},
        "objects": listed.iter().map(|bytes| json!({"sha256": sha(bytes), "bytes": bytes.len(),
            "path": format!("objects/{}", sha(bytes))})).collect::<Vec<_>>()}))
    .unwrap();
    let mut responses = vec![
        (ok(manifest.as_bytes()), NO_HOLD),
        (ok(release.as_bytes()), NO_HOLD),
        (ok(envelope.as_bytes()), NO_HOLD),
        // The origin publishes no transfer index.
        (delta::http(404, b""), NO_HOLD),
    ];
    responses.extend(listed.iter().map(|bytes| (ok(bytes), NO_HOLD)));
    let server = serve(listener, responses);
    let acq = acquisition(&dir, &store, &keys);
    let selected = acq
        .check(&format!("{origin}/update/"))
        .unwrap()
        .selected
        .unwrap();
    let ready = acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    for (request, bytes) in requests[4..].iter().zip(&listed) {
        assert!(
            request.starts_with(&format!("GET /updates/objects/{} ", sha(bytes))),
            "{request}"
        );
    }
    assert_in_volumes(&device, &d);
    assert_eq!(
        fs::read_dir(&ready.objects).unwrap().count(),
        1,
        "only the signature is staged"
    );
}
