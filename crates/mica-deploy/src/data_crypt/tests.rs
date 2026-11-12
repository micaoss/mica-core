use std::fs::{File, OpenOptions};

use serde_json::json;

use super::*;

/// A 20 MiB sparse file standing in for the DATA partition.
fn device() -> (tempfile::TempDir, File) {
    let dir = tempfile::tempdir().unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.path().join("data.img"))
        .unwrap();
    file.set_len(20 * 1024 * 1024).unwrap();
    (dir, file)
}

/// argon2id at the smallest cost, so the tests stay fast.
const ARGON: Kdf = Kdf::Argon2id {
    time: 1,
    memory_kib: 64,
    cpus: 1,
};

fn token() -> Map<String, Value> {
    json!({ "type": "systemd-tpm2", "tpm2-pcrs": [7, 15], "mica-srk-name": "000b00" })
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn xts_round_trips_and_differs_per_sector() {
    let key: Vec<u8> = (0..64).collect();
    let mut data = vec![7u8; 1024];
    xts::encrypt(&key, &mut data).unwrap();
    assert_ne!(data[..512], data[512..], "the sector number is the tweak");
    xts::decrypt(&key, &mut data).unwrap();
    assert_eq!(data, vec![7u8; 1024]);
    assert!(xts::encrypt(&key, &mut [0u8; 100]).is_err());
    assert!(xts::encrypt(&key[..32], &mut [0u8; 512]).is_err());
}

/// A fixed vector, taken from this implementation once it matched cryptsetup
/// both ways (the ignored cross-checks below): two sectors, so the second
/// sector's tweak is pinned too.
#[test]
fn xts_matches_its_cross_checked_vector() {
    let key: Vec<u8> = (0..64).collect();
    let mut data: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    xts::encrypt(&key, &mut data).unwrap();
    assert_eq!(
        hex::encode(digest(&SHA256, &data)),
        "32a9ac00dfd4bcf98cd14d7c04df1750a3ff35a8600e1b489b4683609bc289d3"
    );
}

#[test]
fn af_merges_what_it_split() {
    let key: Vec<u8> = (0..64).collect();
    let material = af::split(&key, 4000).unwrap();
    assert_eq!(material.len(), 64 * 4000);
    assert_eq!(*af::merge(&material, 64, 4000).unwrap(), key);
    let mut damaged = material.clone();
    damaged[10] ^= 1;
    assert_ne!(*af::merge(&damaged, 64, 4000).unwrap(), key);
}

#[test]
fn a_formatted_volume_opens_with_each_keyslot_secret_only() {
    let (_dir, file) = device();
    assert!(!has_header(&file).unwrap());
    let (volume, key) = Volume::format(
        &file,
        Cipher::AesXts,
        "6f1d0c5e-0000-4000-8000-000000000001",
        &[
            NewKeyslot {
                secret: b"sealed-secret-32-bytes-base64...",
                kdf: Kdf::Pbkdf2 { iterations: 1000 },
                token: Some(token()),
            },
            NewKeyslot {
                secret: b"recovery passphrase",
                kdf: ARGON,
                token: None,
            },
        ],
    )
    .unwrap();
    assert_eq!(key.len(), 64);
    assert!(has_header(&file).unwrap());

    let read = Volume::read(&file).unwrap();
    assert_eq!(read.cipher(), Cipher::AesXts);
    assert_eq!(read.uuid(), "6f1d0c5e-0000-4000-8000-000000000001");
    assert_eq!(read.keyslots(), [0, 1]);
    let tokens = read.tokens();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].0["type"], "systemd-tpm2");
    assert_eq!(
        tokens[0].0["mica-srk-name"], "000b00",
        "token fields are kept"
    );
    assert_eq!(tokens[0].1, [0]);

    assert_eq!(
        *read
            .unlock(&file, 0, b"sealed-secret-32-bytes-base64...")
            .unwrap(),
        *key
    );
    assert_eq!(
        *read.unlock(&file, 1, b"recovery passphrase").unwrap(),
        *key
    );
    assert!(
        read.unlock(&file, 1, b"sealed-secret-32-bytes-base64...")
            .is_err()
    );
    assert!(read.unlock(&file, 0, b"wrong").is_err());
    assert_eq!(
        *read.unlock_any(&file, b"recovery passphrase").unwrap(),
        *key
    );
    assert!(read.unlock_any(&file, b"wrong").is_err());
    drop(volume);
}

#[test]
fn keyslots_are_added_and_killed_and_the_last_one_stays() {
    let (_dir, file) = device();
    let (mut volume, key) = Volume::format(
        &file,
        Cipher::Adiantum,
        "u",
        &[NewKeyslot {
            secret: b"one",
            kdf: ARGON,
            token: Some(token()),
        }],
    )
    .unwrap();
    assert_eq!(key.len(), 32);
    let staged = volume
        .add_keyslot(
            &file,
            &key,
            &NewKeyslot {
                secret: b"two",
                kdf: Kdf::Pbkdf2 { iterations: 1000 },
                token: Some(token()),
            },
        )
        .unwrap();
    assert_eq!(staged, 1);
    // A key that is not the volume's adds nothing.
    assert!(
        volume
            .add_keyslot(
                &file,
                &[0u8; 32],
                &NewKeyslot {
                    secret: b"x",
                    kdf: ARGON,
                    token: None
                }
            )
            .is_err()
    );
    let read = Volume::read(&file).unwrap();
    assert_eq!(read.cipher(), Cipher::Adiantum);
    assert_eq!(read.tokens().len(), 2);
    assert_eq!(*read.unlock(&file, 1, b"two").unwrap(), *key);

    volume.kill_keyslot(&file, 0).unwrap();
    let read = Volume::read(&file).unwrap();
    assert_eq!(read.keyslots(), [1]);
    assert_eq!(
        read.tokens().len(),
        1,
        "the token of the killed keyslot goes with it"
    );
    assert_eq!(read.tokens()[0].1, [1]);
    assert!(read.unlock_any(&file, b"one").is_err());
    assert!(
        volume.kill_keyslot(&file, 1).is_err(),
        "the last keyslot stays"
    );
    // The freed area is reused.
    assert_eq!(
        volume
            .add_keyslot(
                &file,
                &key,
                &NewKeyslot {
                    secret: b"three",
                    kdf: ARGON,
                    token: None
                }
            )
            .unwrap(),
        0
    );
    assert_eq!(
        *Volume::read(&file)
            .unwrap()
            .unlock(&file, 0, b"three")
            .unwrap(),
        *key
    );
}

#[test]
fn a_torn_copy_leaves_the_other_and_the_newest_valid_copy_wins() {
    let (_dir, file) = device();
    let (mut volume, key) = Volume::format(
        &file,
        Cipher::AesXts,
        "u",
        &[NewKeyslot {
            secret: b"one",
            kdf: ARGON,
            token: None,
        }],
    )
    .unwrap();
    volume
        .add_keyslot(
            &file,
            &key,
            &NewKeyslot {
                secret: b"two",
                kdf: ARGON,
                token: None,
            },
        )
        .unwrap();
    // The primary torn mid-write: the secondary, same seqid, stands.
    file.write_all_at(&[0xff; 100], 5000).unwrap();
    let read = Volume::read(&file).unwrap();
    assert_eq!(read.keyslots(), [0, 1]);
    // A secondary from an older write, primary good: the newest wins.
    let mut old = vec![0u8; HEADER_SIZE as usize];
    let (_dir2, other) = device();
    let (_v, _k) = Volume::format(
        &other,
        Cipher::AesXts,
        "u",
        &[NewKeyslot {
            secret: b"one",
            kdf: ARGON,
            token: None,
        }],
    )
    .unwrap();
    other.read_exact_at(&mut old, HEADER_SIZE).unwrap();
    volume
        .add_keyslot(
            &file,
            &key,
            &NewKeyslot {
                secret: b"three",
                kdf: ARGON,
                token: None,
            },
        )
        .unwrap();
    file.write_all_at(&old, HEADER_SIZE).unwrap();
    assert_eq!(Volume::read(&file).unwrap().keyslots(), [0, 1, 2]);
    // Both torn: no volume, and still a header (never formatted as plain).
    file.write_all_at(&[0xff; 100], 5000).unwrap();
    file.write_all_at(&[0xff; 100], HEADER_SIZE + 5000).unwrap();
    assert!(Volume::read(&file).is_err());
    assert!(has_header(&file).unwrap());
}

#[test]
fn a_header_outside_the_profile_is_refused() {
    let (_dir, file) = device();
    let (mut volume, _key) = Volume::format(
        &file,
        Cipher::AesXts,
        "u",
        &[NewKeyslot {
            secret: b"one",
            kdf: ARGON,
            token: None,
        }],
    )
    .unwrap();
    volume.metadata.segments.get_mut("0").unwrap().encryption = "aes-cbc-essiv:sha256".into();
    volume.write(&file).unwrap();
    assert!(Volume::read(&file).is_err());
    volume.metadata.segments.get_mut("0").unwrap().encryption = Cipher::AesXts.spec().into();
    volume.metadata.segments.get_mut("0").unwrap().sector_size = 512;
    volume.write(&file).unwrap();
    assert!(Volume::read(&file).is_err());
}

// Cross-checks with cryptsetup, run by hand in a container that has it (not in
// CI): `MICA_XCHECK_DIR` is shared with the cryptsetup side.

fn xcheck_dir() -> std::path::PathBuf {
    std::env::var_os("MICA_XCHECK_DIR")
        .expect("MICA_XCHECK_DIR")
        .into()
}

/// Volumes made here, for cryptsetup to dump and open.
#[test]
#[ignore = "cryptsetup cross-check"]
fn cross_check_write() {
    let dir = xcheck_dir();
    for (name, cipher) in [("aes", Cipher::AesXts), ("adi", Cipher::Adiantum)] {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(format!("{name}.img")))
            .unwrap();
        file.set_len(20 * 1024 * 1024).unwrap();
        let (_volume, key) = Volume::format(
            &file,
            cipher,
            "6f1d0c5e-0000-4000-8000-000000000001",
            &[
                NewKeyslot {
                    secret: b"tpm-secret",
                    kdf: Kdf::Pbkdf2 { iterations: 1000 },
                    token: Some(token()),
                },
                NewKeyslot {
                    secret: b"recovery passphrase",
                    kdf: Kdf::Argon2id {
                        time: 4,
                        memory_kib: 65536,
                        cpus: 1,
                    },
                    token: None,
                },
            ],
        )
        .unwrap();
        std::fs::write(dir.join(format!("{name}.key")), hex::encode(&*key)).unwrap();
    }
}

/// Volumes cryptsetup made, with their volume keys as it dumped them.
#[test]
#[ignore = "cryptsetup cross-check"]
fn cross_check_read() {
    let dir = xcheck_dir();
    for name in ["cs-aes"] {
        let file = File::open(dir.join(format!("{name}.img"))).unwrap();
        let volume = Volume::read(&file).unwrap();
        let expected = std::fs::read_to_string(dir.join(format!("{name}.cskey"))).unwrap();
        for secret in [&b"recovery passphrase"[..], b"tpm-secret"] {
            let key = volume.unlock_any(&file, secret).unwrap();
            assert_eq!(hex::encode(&*key), expected.trim(), "{name}");
        }
    }
}
