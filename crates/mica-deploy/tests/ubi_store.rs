//! The UBI object store of a `ubi` board: its boot records, its objects in
//! static volumes, the device as sysfs describes it, and the NAND loader.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/ubi.rs"]
mod ubi;

use aws_lc_rs::digest;
use mica_deploy::{
    components::Artifact,
    fit_env::{ENV_SIZE, Environment, Record, encode},
    ubi::{SysUbi, UbiDevice, VolumeKind, find, publish_object, verify_object},
};
use std::fs;
use tempfile::TempDir;
use ubi::DirUbi;

fn records(tries: Option<u8>) -> Vec<Record> {
    vec![Record {
        id: "a".repeat(64),
        kernel_id: "c".repeat(64),
        generation: 1,
        tries_left: tries,
    }]
}

fn put_copy(device: &DirUbi, name: &str, bytes: &[u8]) {
    let volume = find(device, name).unwrap().unwrap();
    fs::write(device.path(volume.id), bytes).unwrap();
}

fn artifact(bytes: &[u8]) -> Artifact {
    Artifact {
        sha256: hex::encode(digest::digest(&digest::SHA256, bytes)),
        bytes: bytes.len() as u64,
    }
}

/// The copies are LEB 0 of `records-a` and `records-b`, chosen by U-Boot's
/// rule; a save changes the inactive copy only and reads it back.
#[test]
fn records_alternate_between_the_two_record_volumes() {
    let dir = TempDir::new().unwrap();
    let device = DirUbi::with_fixed_volumes(dir.path(), 256);
    put_copy(&device, "records-a", &encode(&records(Some(3)), 0).unwrap());
    put_copy(&device, "records-b", &encode(&records(Some(3)), 1).unwrap());
    let mut env = Environment::load_volumes(&device).unwrap();
    assert_eq!((env.slot, env.flag), (1, 1));
    env.records[0].tries_left = Some(2);
    let untouched = device.read("records-b");
    env.save_volumes(&device).unwrap();
    assert_eq!(device.read("records-b"), untouched);
    assert_eq!(device.read("records-a").len(), ENV_SIZE);
    let saved = Environment::load_volumes(&device).unwrap();
    assert_eq!((saved.slot, saved.flag), (0, 2));
    assert_eq!(saved.records[0].tries_left, Some(2));
    // A partition's save is not a volume's.
    assert!(saved.save(&dir.path().join("firmware.img")).is_err());
}

/// A copy whose volume cannot be read whole is invalid, as a copy with a bad
/// CRC is, and the other copy holds; with neither, there are no records.
#[test]
fn an_unreadable_record_volume_is_an_invalid_copy() {
    for spoil in ["corrupted", "updating", "short", "absent"] {
        let dir = TempDir::new().unwrap();
        let device = DirUbi::with_fixed_volumes(dir.path(), 256);
        put_copy(&device, "records-a", &encode(&records(Some(3)), 0).unwrap());
        put_copy(&device, "records-b", &encode(&records(None), 1).unwrap());
        match spoil {
            "corrupted" => device.corrupt("records-b"),
            "updating" => device.interrupt_update("records-b"),
            "short" => put_copy(&device, "records-b", &[0; 4096]),
            _ => {
                let volume = find(&device, "records-b").unwrap().unwrap();
                device.remove(volume.id).unwrap();
            }
        }
        let env = Environment::load_volumes(&device).unwrap();
        assert_eq!(
            (env.slot, env.records[0].tries_left),
            (0, Some(3)),
            "{spoil}"
        );
        put_copy(&device, "records-a", &[0; ENV_SIZE]);
        assert!(Environment::load_volumes(&device).is_err(), "{spoil}");
    }
}

/// A static record volume, or one whose erase block cannot hold a copy, is
/// not written.
#[test]
fn records_are_written_only_to_a_dynamic_volume() {
    let dir = TempDir::new().unwrap();
    let device = DirUbi::create(dir.path(), 256);
    device.put_static("records-a", &encode(&records(None), 0).unwrap());
    device
        .create("records-b", ubi::LEB, VolumeKind::Dynamic)
        .unwrap();
    put_copy(&device, "records-b", &encode(&records(None), 1).unwrap());
    let env = Environment::load_volumes(&device).unwrap();
    assert_eq!(env.slot, 1, "a static copy is not a record copy");
    let error = env.save_volumes(&device).unwrap_err();
    assert!(
        error.to_string().contains("invalid boot record volume"),
        "{error:#}"
    );
}

/// An object is written to `p.<name>` at its signed length and renamed into
/// place only once read back against its digest; a leftover pending volume
/// is replaced, and an existing object is verified, never rewritten.
#[test]
fn objects_are_published_through_a_pending_volume() {
    let dir = TempDir::new().unwrap();
    let device = DirUbi::with_fixed_volumes(dir.path(), 256);
    let payload = vec![7_u8; 300_000];
    let source = dir.path().join("source");
    fs::write(&source, &payload).unwrap();
    let signed = artifact(&payload);
    device.put_static("p.r-1", b"leftover");
    publish_object(&device, &source, "r-1", &signed).unwrap();
    let volume = find(&device, "r-1").unwrap().unwrap();
    assert_eq!(
        (volume.kind, volume.data_bytes, volume.reserved_lebs),
        (VolumeKind::Static, payload.len() as u64, 3)
    );
    assert!(find(&device, "p.r-1").unwrap().is_none());
    assert_eq!(device.read("r-1"), payload);
    publish_object(&device, &source, "r-1", &signed).unwrap();
    assert_eq!(find(&device, "r-1").unwrap().unwrap().id, volume.id);

    // A source that is not the signed object is never renamed into place.
    fs::write(&source, vec![8_u8; payload.len()]).unwrap();
    let error = publish_object(&device, &source, "r-2", &signed).unwrap_err();
    assert!(error.to_string().contains("digest mismatch"), "{error:#}");
    assert!(find(&device, "r-2").unwrap().is_none());
    assert!(find(&device, "p.r-2").unwrap().is_some());
}

/// A held object is reused only when it is static, sound, of the signed
/// length and of the signed digest.
#[test]
fn a_held_object_is_held_to_its_signature() {
    let payload = b"object bytes".to_vec();
    for spoil in [
        "none",
        "corrupted",
        "updating",
        "dynamic",
        "length",
        "digest",
    ] {
        let dir = TempDir::new().unwrap();
        let device = DirUbi::create(dir.path(), 256);
        match spoil {
            "dynamic" => {
                device
                    .create("c-1", payload.len() as u64, VolumeKind::Dynamic)
                    .unwrap();
            }
            "length" => {
                device.put_static("c-1", &payload[1..]);
            }
            "digest" => {
                device.put_static("c-1", &vec![0; payload.len()]);
            }
            _ => {
                device.put_static("c-1", &payload);
            }
        }
        match spoil {
            "corrupted" => device.corrupt("c-1"),
            "updating" => device.interrupt_update("c-1"),
            _ => {}
        }
        assert_eq!(
            verify_object(&device, "c-1", &artifact(&payload)).is_ok(),
            spoil == "none",
            "{spoil}"
        );
    }
}

/// The device is the one sysfs says holds the policy's MTD partition, with
/// its volumes as sysfs lists them.
#[test]
fn the_attached_device_and_its_volumes_are_read_from_sysfs() {
    let dir = TempDir::new().unwrap();
    let class = dir.path().join("class");
    let write = |path: &str, value: &str| {
        let path = class.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{value}\n")).unwrap();
    };
    write("mtd/mtd0/name", "u-boot");
    write("mtd/mtd1/name", "ubi");
    write("ubi/ubi0/mtd_num", "1");
    write("ubi/ubi0/eraseblock_size", "126976");
    write("ubi/ubi0/avail_eraseblocks", "200");
    for (id, name, kind, bytes, lebs, corrupted, updating) in [
        (0, "system", "dynamic", "8126464", "64", "0", "0"),
        (3, "r-1", "static", "300000", "3", "0", "1"),
    ] {
        let volume = format!("ubi/ubi0_{id}");
        write(&format!("{volume}/name"), name);
        write(&format!("{volume}/type"), kind);
        write(&format!("{volume}/data_bytes"), bytes);
        write(&format!("{volume}/reserved_ebs"), lebs);
        write(&format!("{volume}/corrupted"), corrupted);
        write(&format!("{volume}/upd_marker"), updating);
    }
    let device = SysUbi::under(&class, &dir.path().join("dev"), "ubi").unwrap();
    assert_eq!((device.number(), device.leb_bytes()), (0, 126_976));
    assert_eq!(device.available_lebs().unwrap(), 200);
    let volumes = device.volumes().unwrap();
    assert_eq!(volumes.len(), 2);
    assert_eq!(
        (&volumes[1].name, volumes[1].kind, volumes[1].data_bytes),
        (&"r-1".to_owned(), VolumeKind::Static, 300_000)
    );
    assert!(volumes[0].sound() && !volumes[1].sound());
    assert_eq!(device.path(3), dir.path().join("dev/ubi0_3"));
    // A node that is not the character device sysfs names is not opened.
    fs::create_dir_all(dir.path().join("dev")).unwrap();
    fs::write(dir.path().join("dev/ubi0_3"), b"").unwrap();
    write("ubi/ubi0_3/dev", "250:4");
    assert!(device.open(3, false).is_err());

    for absent in ["u-boot", "nand", "bad name"] {
        assert!(SysUbi::under(&class, &dir.path().join("dev"), absent).is_err());
    }
    write("ubi/ubi0/eraseblock_size", "512");
    assert!(SysUbi::under(&class, &dir.path().join("dev"), "ubi").is_err());
}

/// The loader is read the way the boot ROM reads it: erase block by erase
/// block, passing over the bad ones, and never past its partition.
#[test]
fn the_mtd_loader_is_read_past_bad_erase_blocks() {
    use mica_deploy::firmware::read_skipping_bad;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mtd");
    let erase = 4096_u64;
    let mut image = Vec::new();
    for block in 0..6_u8 {
        image.extend(vec![block; erase as usize]);
    }
    fs::write(&path, &image).unwrap();
    let file = fs::File::open(&path).unwrap();
    let bad = |offset: u64| Ok(offset == erase || offset == 3 * erase);
    let loader = read_skipping_bad(&file, erase, 6 * erase, 2 * erase + 100, bad).unwrap();
    let mut expected = vec![0_u8; erase as usize];
    expected.extend(vec![2_u8; erase as usize]);
    expected.extend(vec![4_u8; 100]);
    assert_eq!(loader, expected);
    let error = read_skipping_bad(&file, erase, 6 * erase, 5 * erase, bad).unwrap_err();
    assert!(
        error.to_string().contains("past its MTD partition"),
        "{error:#}"
    );
    assert!(
        read_skipping_bad(&file, erase, 6 * erase, erase, |_| anyhow::bail!(
            "no answer"
        ))
        .is_err()
    );
}

/// The loader's partition is found by name in sysfs, and its node is held to
/// the identity sysfs gives it.
#[test]
fn the_mtd_loader_partition_is_found_by_name() {
    use mica_deploy::firmware::read_mtd_loader;
    let dir = TempDir::new().unwrap();
    let class = dir.path().join("mtd");
    for (node, name) in [("mtd0", "u-boot"), ("mtd1", "ubi")] {
        fs::create_dir_all(class.join(node)).unwrap();
        fs::write(class.join(node).join("name"), format!("{name}\n")).unwrap();
        fs::write(class.join(node).join("erasesize"), "131072\n").unwrap();
        fs::write(class.join(node).join("size"), "1048576\n").unwrap();
        fs::write(class.join(node).join("dev"), "90:0\n").unwrap();
    }
    fs::create_dir_all(dir.path().join("dev")).unwrap();
    fs::write(dir.path().join("dev/mtd0"), vec![0; 1048576]).unwrap();
    let error = read_mtd_loader(&class, &dir.path().join("dev"), "u-boot", 4096).unwrap_err();
    assert!(error.to_string().contains("identity"), "{error:#}");
    let error = read_mtd_loader(&class, &dir.path().join("dev"), "spl", 4096).unwrap_err();
    assert!(
        error.to_string().contains("absent or ambiguous"),
        "{error:#}"
    );
    assert!(read_mtd_loader(&class, &dir.path().join("dev"), "", 4096).is_err());
}
