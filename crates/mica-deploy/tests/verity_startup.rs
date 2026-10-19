// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mica_deploy::{
    boot::{
        startup::verity::{known_provider, table, verify_table},
        verity_provider_majors,
    },
    components::VerityImage,
};

fn image() -> VerityImage {
    serde_json::from_value(serde_json::json!({
        "image": {"bytes": 12288, "sha256": "a".repeat(64)},
        "rootHash": "b".repeat(64),
        "signature": {"bytes": 1024, "sha256": "c".repeat(64)},
        "verity": {"version": 1, "algorithm": "sha256", "dataBlockSize": 4096,
            "hashBlockSize": 4096, "dataBlocks": 2, "hashOffset": 8192, "salt": "d".repeat(64)}
    }))
    .unwrap()
}

#[test]
fn signed_geometry_translates_to_one_complete_verity_target() {
    let image = image();
    let target = table(7, 3, "mica-root-signature", &image).unwrap();
    assert_eq!(target.sector, 0);
    assert_eq!(target.length, 16);
    assert_eq!(target.kind, "verity");
    assert_eq!(
        target.parameters,
        format!(
            "1 7:3 7:3 4096 4096 2 2 sha256 {} {} 3 panic_on_corruption root_hash_sig_key_desc mica-root-signature",
            image.root_hash, image.verity.salt
        )
    );
    verify_table(&target, std::slice::from_ref(&target)).unwrap();
    for mutation in ["signature", "target", "length", "root", "panic"] {
        let mut changed = target.clone();
        match mutation {
            "signature" => {
                changed.parameters = changed
                    .parameters
                    .replace("root_hash_sig_key_desc", "ignored")
            }
            "target" => changed.kind = "linear".into(),
            "length" => changed.length -= 1,
            "root" => {
                changed.parameters = changed
                    .parameters
                    .replace(&image.root_hash, &"e".repeat(64))
            }
            _ => {
                changed.parameters = changed
                    .parameters
                    .replace("panic_on_corruption", "ignore_corruption")
            }
        }
        assert!(verify_table(&target, &[changed]).is_err(), "{mutation}");
    }
    assert!(verify_table(&target, &[]).is_err());
}

#[test]
fn no_geometry_fallback_or_optional_signature() {
    for mutation in 0..9 {
        let mut image = image();
        match mutation {
            0 => image.verity.version = 0,
            1 => image.verity.algorithm = "sha1".into(),
            2 => image.verity.data_block_size = 512,
            3 => image.verity.hash_block_size = 512,
            4 => image.verity.hash_offset += 1,
            5 => image.verity.data_blocks = 0,
            6 => image.verity.data_blocks = u64::MAX,
            7 => image.root_hash = "invalid".into(),
            _ => image.verity.salt = "-".into(),
        }
        assert!(
            table(7, 0, "mica-root-signature", &image).is_err(),
            "{mutation}"
        );
    }
    for description in ["", "has space", "has\0nul"] {
        assert!(table(7, 0, description, &image()).is_err());
    }
}

/// A UBI board's provider is the `ubiblock` of a static volume, whose major
/// the kernel assigns at registration: the table takes it as it takes a loop.
#[test]
fn a_ubiblock_provider_translates_as_a_loop_does() {
    let target = table(254, 6, "cryptsetup:mica-root", &image()).unwrap();
    assert!(
        target
            .parameters
            .starts_with("1 254:6 254:6 4096 4096 2 2 sha256 ")
    );
}

/// A provider is a loop device of major 7, or a `ubiblockN_M` of the major
/// `/proc/devices` lists for `ubiblock`; nothing else, whatever its name.
#[test]
fn providers_are_loop_devices_or_registered_ubiblocks() {
    let providers = verity_provider_majors(
        "Character devices:\n  1 mem\n247 ubi0\n\nBlock devices:\n  7 loop\n254 ubiblock\n259 blkext\n",
    )
    .unwrap();
    assert_eq!(providers, [7, 254]);
    assert!(known_provider("loop0", 7, &providers));
    assert!(known_provider("ubiblock0_6", 254, &providers));
    assert!(!known_provider("ubiblock0_6", 259, &providers));
    assert!(!known_provider("ubiblock0_6", 254, &[7]));
    assert!(!known_provider("ubiblock0_6", 7, &providers));
    assert!(!known_provider("loop0", 254, &providers));
    assert!(!known_provider("mmcblk0p2", 254, &providers));
    assert!(!known_provider("ubiblock0", 254, &providers));
    assert!(!known_provider("loop", 7, &providers));
    assert_eq!(
        verity_provider_majors("Block devices:\n  7 loop\n").unwrap(),
        [7]
    );
}
