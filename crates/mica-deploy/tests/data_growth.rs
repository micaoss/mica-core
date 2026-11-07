//! DATA's growth at the first boot of a `ubi` board (`docs/mica-core.md`
//! 6.6): the empty `data` volume takes the device's free LEBs less the store
//! reserve, once, and a formatted one keeps its size.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/ubi.rs"]
mod ubi;

use mica_deploy::ubi::{DataGrowth, UbiDevice, VolumeKind, find, grow_data, space, wipe_data};
use std::fs;
use tempfile::TempDir;
use ubi::{DirUbi, LEB};

const MIB: u64 = 1024 * 1024;

/// A 600-LEB device with the fixed volumes (130 LEBs, DATA 64 of them).
fn device(dir: &TempDir) -> DirUbi {
    DirUbi::with_fixed_volumes(dir.path(), 600)
}

fn data_lebs(device: &DirUbi) -> u64 {
    find(device, "data").unwrap().unwrap().reserved_lebs
}

#[test]
fn an_empty_data_takes_the_free_lebs_less_the_reserve_once() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);
    assert_eq!(device.available_lebs().unwrap(), 470);

    // 32 MiB is 264.3 LEBs of 126976 bytes, kept as 265.
    let grown = grow_data(&device, 32 * MIB).unwrap();
    assert_eq!(grown, DataGrowth::Grown { from: 64, to: 269 });
    assert_eq!(data_lebs(&device), 269);
    assert_eq!(device.available_lebs().unwrap(), 265);

    // Again, as after a power cut before the mount: nothing is left to give.
    let again = grow_data(&device, 32 * MIB).unwrap();
    assert_eq!(
        again,
        DataGrowth::Kept {
            lebs: 269,
            available: 265,
            reserve: 265
        }
    );
    assert_eq!(data_lebs(&device), 269);
}

#[test]
fn a_formatted_data_keeps_its_size() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);
    let data = find(&device, "data").unwrap().unwrap();
    // Anything written reaches LEB 0, as UBIFS's format does.
    fs::write(device.path(data.id), b"UBI#").unwrap();

    assert_eq!(
        grow_data(&device, MIB).unwrap(),
        DataGrowth::Formatted { lebs: 64 }
    );
    assert_eq!(data_lebs(&device), 64);
    assert_eq!(device.available_lebs().unwrap(), 470);
}

#[test]
fn a_mapped_leb_past_the_first_also_counts() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);
    let data = find(&device, "data").unwrap().unwrap();
    let file = device.open(data.id, true).unwrap();
    file.set_len(40 * LEB + 1).unwrap();
    drop(file);

    assert_eq!(
        grow_data(&device, MIB).unwrap(),
        DataGrowth::Formatted { lebs: 64 }
    );
}

#[test]
fn too_little_beside_the_reserve_keeps_the_size() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);

    // 470 free; 64 MiB is 529 LEBs.
    assert_eq!(
        grow_data(&device, 64 * MIB).unwrap(),
        DataGrowth::Kept {
            lebs: 64,
            available: 470,
            reserve: 529
        }
    );
    // Exactly the reserve free leaves nothing to give either.
    assert_eq!(
        grow_data(&device, 470 * LEB).unwrap(),
        DataGrowth::Kept {
            lebs: 64,
            available: 470,
            reserve: 470
        }
    );
    assert_eq!(data_lebs(&device), 64);
    // One LEB more than the reserve is one LEB of growth.
    assert_eq!(
        grow_data(&device, 469 * LEB).unwrap(),
        DataGrowth::Grown { from: 64, to: 65 }
    );
}

#[test]
fn a_data_volume_that_is_absent_static_or_unsound_is_refused() {
    let dir = TempDir::new().unwrap();
    let device = DirUbi::create(dir.path(), 600);
    assert!(grow_data(&device, MIB).is_err());

    let dir = TempDir::new().unwrap();
    let device = DirUbi::create(dir.path(), 600);
    device.put_static("data", b"image");
    let error = format!("{:#}", grow_data(&device, MIB).unwrap_err());
    assert!(error.contains("not a sound dynamic volume"), "{error}");
    assert_eq!(
        find(&device, "data").unwrap().unwrap().kind,
        VolumeKind::Static
    );
}

#[test]
fn status_reports_the_space_in_lebs() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);
    assert_eq!(
        space(&device, 4 * MIB, Some(64 * MIB)).unwrap(),
        serde_json::json!({
            "lebBytes": LEB,
            "availableLebs": 470,
            "dataLebs": 64,
            "systemReserveLebs": 34,
            "storeReserveLebs": 529,
        })
    );
    let without = space(&device, 4 * MIB, None).unwrap();
    assert!(without.get("storeReserveLebs").is_none());
}

/// A factory reset empties a formatted `data` volume and keeps its size, so
/// the growth after it sees an empty volume and nothing else changes.
#[test]
fn a_factory_reset_empties_data_and_keeps_its_size() {
    let dir = TempDir::new().unwrap();
    let device = device(&dir);
    assert_eq!(
        grow_data(&device, 32 * MIB).unwrap(),
        DataGrowth::Grown { from: 64, to: 269 }
    );
    let data = find(&device, "data").unwrap().unwrap();
    fs::write(device.path(data.id), vec![1; 3 * LEB as usize]).unwrap();
    let others: Vec<_> = device
        .names()
        .into_iter()
        .filter(|name| name != "data")
        .map(|name| (device.read(&name), name))
        .collect();

    assert_eq!(wipe_data(&device).unwrap(), 269);
    let data = find(&device, "data").unwrap().unwrap();
    assert!(data.sound());
    assert_eq!(data.kind, VolumeKind::Dynamic);
    assert_eq!(data.reserved_lebs, 269);
    assert_eq!(fs::metadata(device.path(data.id)).unwrap().len(), 0);
    for (bytes, name) in others {
        assert_eq!(device.read(&name), bytes, "{name}");
    }
    // The growth after it finds an empty volume with nothing left to give.
    assert!(matches!(
        grow_data(&device, 32 * MIB).unwrap(),
        DataGrowth::Kept { lebs: 269, .. }
    ));
    // Interrupted and run again, it is the same.
    assert_eq!(wipe_data(&device).unwrap(), 269);

    let dir = TempDir::new().unwrap();
    let device = DirUbi::create(dir.path(), 600);
    assert!(wipe_data(&device).is_err());
    device.put_static("data", b"image");
    let error = format!("{:#}", wipe_data(&device).unwrap_err());
    assert!(error.contains("not a dynamic volume"), "{error}");
}
