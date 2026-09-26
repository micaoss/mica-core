#[path = "support/boards.rs"]
mod boards;

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::firmware::{
    Firmware, admit_firmware, authenticate_firmware, parse_firmware, verify_installed,
};
use serde_json::{Value, json};

/// What a device running `board` does with a manifest: its shape, then its
/// board's signed policy.
fn read_on(board: &str, manifest: &Value) -> anyhow::Result<Firmware> {
    let firmware = parse_firmware(&serde_json::to_vec(manifest)?)?;
    admit_firmware(&firmware, &boards::identity(board), &boards::policy(board))?;
    Ok(firmware)
}

fn golden() -> Value {
    serde_json::from_str(include_str!("component-contracts/firmware.json")).unwrap()
}

#[test]
fn firmware_manifests_match_the_shared_signed_contract() {
    let fixture = golden();
    let key: [u8; 32] = STANDARD
        .decode(fixture["publicKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    for record in fixture["records"].as_array().unwrap() {
        let envelope = record["envelope"].as_str().unwrap().as_bytes();
        let firmware = authenticate_firmware(envelope, &[key]).unwrap();
        assert_eq!(serde_json::to_value(&firmware).unwrap(), record["manifest"]);
        assert!(authenticate_firmware(envelope, &[[0; 32]]).is_err());
        let mut bad: Value = serde_json::from_slice(envelope).unwrap();
        bad["signature"] = json!(STANDARD.encode([0; 64]));
        assert!(authenticate_firmware(&serde_json::to_vec(&bad).unwrap(), &[key]).is_err());
    }
}

/// Every golden record is the firmware of exactly one board: its own board's
/// policy admits it, and every other board's refuses it. No table here or in
/// the device says which is which; the policies do.
#[test]
fn each_golden_record_is_admitted_by_its_own_board_alone() {
    for record in golden()["records"].as_array().unwrap() {
        let manifest = &record["manifest"];
        let own = manifest["board"].as_str().unwrap();
        for board in boards::names() {
            assert_eq!(
                read_on(&board, manifest).is_ok(),
                board == own,
                "{own}'s firmware on {board}"
            );
        }
    }
}

#[test]
fn firmware_constraints_reject_out_of_range_and_cross_board_writes() {
    let fixture = golden();
    let original = fixture["records"][2]["manifest"].clone();
    let board = original["board"].as_str().unwrap().to_owned();
    let mutate = |field: &str, value: Value| {
        let mut manifest = original.clone();
        manifest[field] = value;
        manifest["id"] = json!(mica_deploy::components::component_id(&manifest).unwrap());
        manifest
    };
    // Refused by the manifest's own shape, before any board is consulted.
    for (field, value) in [
        ("generation", json!(0)),
        ("online", json!(true)),
        (
            "artifact",
            json!({"bytes":16744449,"sha256":"a".repeat(64)}),
        ),
        ("board", json!("Unknown Board")),
    ] {
        assert!(
            parse_firmware(&serde_json::to_vec(&mutate(field, value)).unwrap()).is_err(),
            "{field}"
        );
    }
    // Well formed, and refused by the device: another architecture, another
    // board, or a write range other than the one its signed policy names.
    for (field, value) in [
        ("arch", json!("amd64")),
        ("board", json!("unknown")),
        // A range aimed at the boot records, long enough for the artifact.
        (
            "target",
            json!({"format":"disk-range","diskOffset":16777216,"maxBytes":1048576}),
        ),
        (
            "target",
            json!({"format":"efi","partition":2,"path":"EFI/BOOT/BOOTAA64.EFI"}),
        ),
    ] {
        let manifest = mutate(field, value);
        assert!(
            parse_firmware(&serde_json::to_vec(&manifest).unwrap()).is_ok(),
            "{field} is refused by its shape, not by the device"
        );
        let error = read_on(&board, &manifest).expect_err(field).to_string();
        assert!(
            error.contains("firmware target differs from signed board policy"),
            "{field}: {error}"
        );
    }
    let mut payload = serde_json::to_vec(&original).unwrap();
    payload.push(b'\n');
    assert!(parse_firmware(&payload).is_err());
}

#[test]
fn firmware_readback_checks_only_the_bound_loader_bytes_and_never_changes_counters() {
    use mica_deploy::deployments::BootBackend;
    use std::fs;
    let directory = tempfile::TempDir::new().unwrap();
    let bytes = b"isolated loader";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    for index in [0, 2] {
        let mut value = golden()["records"][index]["manifest"].clone();
        value["artifact"] = json!({"bytes":bytes.len(),"sha256":digest});
        value["id"] = json!(mica_deploy::components::component_id(&value).unwrap());
        let manifest = parse_firmware(&serde_json::to_vec(&value).unwrap()).unwrap();
        let (boot, path) = if index == 0 {
            let esp = directory.path().join("esp");
            let file = esp.join("EFI/BOOT/BOOTX64.EFI");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, bytes).unwrap();
            (BootBackend::Uefi { esp }, file)
        } else {
            let file = directory.path().join("firmware.img");
            let mut image = bytes.to_vec();
            image.extend_from_slice(b"unchanged environment area");
            fs::write(&file, image).unwrap();
            (
                BootBackend::Fit {
                    layout: boards::layout("cx3576"),
                    firmware: file.clone(),
                },
                file,
            )
        };
        let before = fs::read(&path).unwrap();
        verify_installed(&manifest, &boot).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut damaged = before;
        damaged[0] ^= 1;
        fs::write(&path, damaged).unwrap();
        assert!(verify_installed(&manifest, &boot).is_err());
    }
}

#[test]
fn amlogic_receipt_checks_payload_after_the_vendor_header() {
    let directory = tempfile::tempdir().unwrap();
    let payload = b"signed S7D boot payload";
    let digest = hex::encode(ring::digest::digest(&ring::digest::SHA256, payload));
    let mut value = golden()["records"][2]["manifest"].clone();
    value["board"] = json!("s905x5m");
    value["target"] =
        json!({"format":"emmc-boot","area":"boot0","payloadOffset":512,"maxBytes":4193792});
    value["artifact"] = json!({"bytes":payload.len(),"sha256":digest});
    value["id"] = json!(mica_deploy::components::component_id(&value).unwrap());
    let manifest = read_on("s905x5m", &value).unwrap();
    let path = directory.path().join("boot0");
    let mut bytes = vec![42; 512];
    bytes.extend_from_slice(payload);
    std::fs::write(&path, &bytes).unwrap();
    mica_deploy::firmware::verify_emmc_payload(&manifest, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    bytes[0] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    mica_deploy::firmware::verify_emmc_payload(&manifest, &path).unwrap();
    bytes[512] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(mica_deploy::firmware::verify_emmc_payload(&manifest, &path).is_err());
    for target in [
        // Another payload offset, another length, the other boot area: each
        // well formed and not the range this board's policy names.
        json!({"format":"emmc-boot","area":"boot0","payloadOffset":0,"maxBytes":4193792}),
        json!({"format":"emmc-boot","area":"boot0","payloadOffset":512,"maxBytes":4194304}),
        json!({"format":"emmc-boot","area":"boot1","payloadOffset":512,"maxBytes":4193792}),
        // A format name this device does not implement is refused, with no alias.
        json!({"format":"amlogic-boot0","payloadOffset":512,"maxBytes":4193792}),
    ] {
        value["target"] = target;
        value["id"] = json!(mica_deploy::components::component_id(&value).unwrap());
        assert!(read_on("s905x5m", &value).is_err());
    }
}
