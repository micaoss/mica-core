use super::*;
use std::os::unix::fs::PermissionsExt as _;

/// The vectors of `docs/factory-ap-key.md`: source, raw value, code, SSID and
/// key under the secret `0x00..=0x1f`.
const VECTORS: &[(Source, &str, &str, &str, &str)] = &[
    (
        Source::EmmcCid,
        "150100444a54443452a1b2c3d4e5f600",
        "DFN4L34CT8N",
        "mica-789Z",
        "U8A5TU7RT6A6E8TV",
    ),
    (
        Source::DtSerial,
        "a5f2c3d4e5f60718",
        "LSZJ2L3TWFG",
        "mica-95SV",
        "PNA6B6H4FP6VTCR7",
    ),
    (
        Source::DmiSerial,
        "MXQ1234567",
        "Y9E349ETBKD",
        "mica-XE9C",
        "ZYGL2RYLNSEMMJ6E",
    ),
    (
        Source::DmiUuid,
        "4c4c4544-0042-3510-8051-b4c04f4e3332",
        "LVVABBQD8Y4",
        "mica-2Y83",
        "S8QLYFJDZT7BFJ44",
    ),
    (
        Source::Mac,
        "00:1a:2b:3c:4d:5e",
        "RYP73EEMS8X",
        "mica-VST3",
        "WL2K6GAUJMRJWE6U",
    ),
];

fn test_secret() -> Vec<u8> {
    (0u8..32).collect()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

#[test]
fn the_documented_vectors_hold() {
    for (source, raw, code, ssid, key) in VECTORS {
        assert_eq!(code_of(*source, raw), *code, "{}", source.name());
        assert_eq!(ssid_of(code), *ssid);
        assert_eq!(key_of(&test_secret(), code), *key);
    }
}

#[test]
fn a_typed_code_is_read_in_either_form_and_its_check_character_is_verified() {
    assert_eq!(parse_code("dfn4-l34c-t8n").as_deref(), Ok("DFN4L34CT8N"));
    assert_eq!(parse_code("DFN4L34CT8N").as_deref(), Ok("DFN4L34CT8N"));
    assert_eq!(printed("DFN4L34CT8N"), "DFN4-L34C-T8N");
    // One wrong character, two neighbours swapped, a letter outside the alphabet.
    assert!(parse_code("DFN4L34CT9N").is_err());
    assert!(parse_code("FDN4L34CT8N").is_err());
    assert!(parse_code("DFN4L34CT8O").is_err());
    assert!(parse_code("DFN4L34CT8").is_err());
}

#[test]
fn every_single_character_error_is_caught() {
    let code = VECTORS[0].2.as_bytes();
    for index in 0..code.len() {
        for &other in ALPHABET {
            if other == code[index] {
                continue;
            }
            let mut typo = code.to_vec();
            typo[index] = other;
            let typo = String::from_utf8(typo).expect("ascii");
            assert!(parse_code(&typo).is_err(), "{typo}");
        }
    }
}

#[test]
fn the_board_declares_its_source() {
    let root = tempfile::tempdir().expect("tempdir");
    assert!(device_code(root.path()).is_err());
    write(root.path(), CONFIG_PATH, "# cx3576\nsource=emmc-cid\n");
    write(root.path(), "sys/block/mmcblk1/device/type", "SD\n");
    write(root.path(), "sys/block/mmcblk1/device/cid", "ffff\n");
    write(root.path(), "sys/block/mmcblk2/device/type", "MMC\n");
    write(
        root.path(),
        "sys/block/mmcblk2/device/cid",
        "150100444a54443452a1b2c3d4e5f600\n",
    );
    let code = device_code(root.path()).expect("code");
    assert_eq!(code.code, VECTORS[0].2);
    assert_eq!(
        code.to_json(),
        serde_json::json!({"code": "DFN4-L34C-T8N", "source": "emmc-cid"})
    );

    write(root.path(), CONFIG_PATH, "source=serial\n");
    assert!(
        device_code(root.path())
            .unwrap_err()
            .contains("unknown source")
    );
}

#[test]
fn a_placeholder_is_no_value() {
    let root = tempfile::tempdir().expect("tempdir");
    for (path, source, value) in [
        (
            "sys/class/dmi/id/product_serial",
            Source::DmiSerial,
            "To be filled by O.E.M.",
        ),
        (
            "sys/class/dmi/id/product_serial",
            Source::DmiSerial,
            "FFFFFFFF",
        ),
        ("sys/class/dmi/id/product_serial", Source::DmiSerial, "0000"),
        (
            "sys/class/dmi/id/product_uuid",
            Source::DmiUuid,
            "03000200-0400-0500-0006-000700080009",
        ),
        (
            "sys/class/dmi/id/product_uuid",
            Source::DmiUuid,
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ),
        (
            "sys/firmware/devicetree/base/serial-number",
            Source::DtSerial,
            "\0",
        ),
    ] {
        write(root.path(), path, value);
        assert!(read_source(root.path(), source).is_err(), "{value:?}");
    }
    write(
        root.path(),
        "sys/firmware/devicetree/base/serial-number",
        "a5f2c3d4e5f60718\0",
    );
    assert_eq!(
        read_source(root.path(), Source::DtSerial).as_deref(),
        Ok("a5f2c3d4e5f60718")
    );
}

#[test]
fn the_mac_is_the_lowest_onboard_permanent_address() {
    let root = tempfile::tempdir().expect("tempdir");
    let link = |name: &str, device: &str, address: &str, assign: &str| {
        let device_dir = root.path().join(device);
        std::fs::create_dir_all(&device_dir).expect("mkdir");
        let dir = root.path().join("sys/class/net").join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::os::unix::fs::symlink(&device_dir, dir.join("device")).expect("symlink");
        write(&dir, "type", "1\n");
        write(&dir, "address", &format!("{address}\n"));
        write(&dir, "addr_assign_type", &format!("{assign}\n"));
    };
    link(
        "eth1",
        "devices/platform/fe1c0000.ethernet",
        "00:1A:2B:3C:4D:5F",
        "0",
    );
    link(
        "eth0",
        "devices/platform/fe1b0000.ethernet",
        "00:1A:2B:3C:4D:5E",
        "0",
    );
    link(
        "eth2",
        "devices/platform/usb1/1-1",
        "00:11:22:33:44:55",
        "0",
    );
    link(
        "eth3",
        "devices/platform/fe0a0000.ethernet",
        "52:54:00:12:34:56",
        "0",
    );
    link(
        "eth4",
        "devices/platform/fe0b0000.ethernet",
        "00:aa:bb:cc:dd:ee",
        "3",
    );
    assert_eq!(
        read_source(root.path(), Source::Mac).as_deref(),
        Ok("00:1a:2b:3c:4d:5e")
    );
}

#[test]
fn the_product_secret_is_the_owners_alone() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join(SECRET_PATH);
    assert!(product_secret(root.path()).is_err());
    let mut bytes = test_secret();
    bytes.push(b'\n');
    write(root.path(), SECRET_PATH, "");
    std::fs::write(&path, &bytes).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(
        product_secret(root.path())
            .unwrap_err()
            .contains("readable by others")
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert_eq!(product_secret(root.path()), Ok(test_secret()));
    std::fs::write(&path, &bytes[..31]).expect("write");
    assert!(
        product_secret(root.path())
            .unwrap_err()
            .contains("fewer than 32")
    );
}

/// Whoever has the device in hand reads its code on the login prompt: the
/// issue text names it, and a device without a code has none.
#[test]
fn the_code_is_put_on_the_login_prompt() {
    let root = tempfile::tempdir().expect("tempdir");
    let issue = root.path().join("run/mica/device-code.issue");
    write(root.path(), CONFIG_PATH, "source=emmc-cid\n");
    write(root.path(), "sys/block/mmcblk2/device/type", "MMC\n");
    write(
        root.path(),
        "sys/block/mmcblk2/device/cid",
        "150100444a54443452a1b2c3d4e5f600\n",
    );
    announce(root.path(), &issue);
    assert_eq!(
        std::fs::read_to_string(&issue).expect("issue"),
        "Device code: DFN4-L34C-T8N\n\n"
    );

    write(root.path(), CONFIG_PATH, "source=serial\n");
    announce(root.path(), &issue);
    assert!(!issue.exists(), "a device without a code shows none");
}
