//! The shared contract's product and archive cases (`component-contracts/cases.json`),
//! driven through the device side: the product file, `Acquisition::import` of
//! full and partial MICAUPD1 archives, and the product a deployment names.

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    acquisition::Acquisition,
    components::{PRODUCT_FILE, component_id, device_product},
    deployments::{BootBackend, DeploymentStore},
};
use ring::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{Value, json};
use std::{fs, io::Cursor};
use tempfile::TempDir;

const CASES: &str = include_str!("component-contracts/cases.json");
const PAYLOAD: &str = include_str!("component-contracts/deployment.json");
const OBJECTS: [&str; 5] = [
    "/kernel/boot/artifact",
    "/kernel/support/image",
    "/kernel/support/signature",
    "/rootfs/content/image",
    "/rootfs/content/signature",
];

fn cases() -> Value {
    serde_json::from_str(CASES).unwrap()
}

/// The bytes standing in for one descriptor object: distinct per object, and of
/// the length the golden verity geometry gives an image.
fn object(pointer: &str) -> Vec<u8> {
    if pointer == "unnamed" {
        return vec![0xee; 64];
    }
    let index = OBJECTS.iter().position(|p| *p == pointer).unwrap();
    vec![index as u8 + 1; 12288]
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

/// The golden descriptor with real object bytes, signed; `remove` drops a field first.
fn signed(remove: Option<&str>) -> (Vec<u8>, [u8; 32]) {
    let mut d: Value = serde_json::from_str(PAYLOAD).unwrap();
    for pointer in OBJECTS {
        let bytes = object(pointer);
        *d.pointer_mut(pointer).unwrap() = json!({"bytes": bytes.len(), "sha256": sha256(&bytes)});
    }
    d["kernel"]["id"] = component_id(&d["kernel"]).unwrap().into();
    d["rootfs"]["id"] = component_id(&d["rootfs"]).unwrap().into();
    if let Some(pointer) = remove {
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        d.pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let payload = serde_json::to_vec(&d).unwrap();
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha256(&public),
        STANDARD.encode(&payload),
        STANDARD.encode(key.sign(&payload).as_ref())
    );
    (envelope.into_bytes(), public)
}

/// MICAUPD1 carrying exactly the named objects, in order.
fn archive(envelope: &[u8], names: &[&str]) -> Vec<u8> {
    let mut archive = b"MICAUPD1".to_vec();
    archive.extend((envelope.len() as u32).to_be_bytes());
    archive.extend(envelope);
    archive.extend((names.len() as u32).to_be_bytes());
    for name in names {
        let bytes = object(name);
        archive.extend(sha256(&bytes).as_bytes());
        archive.extend((bytes.len() as u64).to_be_bytes());
        archive.extend(bytes);
    }
    archive
}

fn store() -> (TempDir, DeploymentStore) {
    let dir = TempDir::new().unwrap();
    for name in ["system", "esp", "meta"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    fs::create_dir_all(dir.path().join("esp/loader/entries")).unwrap();
    let store = DeploymentStore::new(
        dir.path().join("system"),
        BootBackend::Uefi {
            esp: dir.path().join("esp"),
        },
        dir.path().join("meta"),
    );
    (dir, store)
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect()
}

#[test]
fn the_product_file_names_the_device_product() {
    let cases = cases();
    let product = &cases["product"];
    assert_eq!(product["file"], PRODUCT_FILE);
    assert_eq!(product["key"], "PRODUCT");
    assert_eq!(
        device_product(product["fixture"].as_str().unwrap()).unwrap(),
        product["product"].as_str().unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Value>(PAYLOAD).unwrap()["product"],
        product["product"]
    );
    for text in [
        "",
        "BOARD=uefi-x64\n",
        "PRODUCT=\n",
        "PRODUCT=\"uefi-x64-dev\"\n",
        "PRODUCT=uefi-x64 dev\n",
        "PRODUCT=uefi-x64-dev\nPRODUCT=uefi-x64-prod\n",
        "BOARD=uefi-x64\nFEATURES=\"micad mqtt containers\"\n",
    ] {
        assert!(device_product(text).is_err(), "{text:?}");
    }
}

#[test]
fn product_cases() {
    for case in cases()["productCases"].as_array().unwrap() {
        let (envelope, key) = signed(case["remove"].as_str());
        let (dir, store) = store();
        let keys = [key];
        let acq = Acquisition {
            root: dir.path().join("updates"),
            store: &store,
            keys: &keys,
            board: "uefi-x64",
            arch: "amd64",
            product: case["device"].as_str().unwrap(),
            max_bytes: 1024 * 1024,
        };
        let result = acq.import(&mut Cursor::new(archive(&envelope, &OBJECTS)));
        assert_eq!(
            if result.is_ok() {
                "accepted"
            } else {
                "refused"
            },
            case["result"],
            "{}: {:?}",
            case["name"],
            result.err()
        );
    }
}

#[test]
fn archive_cases() {
    let (envelope, key) = signed(None);
    for case in cases()["archives"].as_array().unwrap() {
        let (dir, store) = store();
        let keys = [key];
        let acq = Acquisition {
            root: dir.path().join("updates"),
            store: &store,
            keys: &keys,
            board: "uefi-x64",
            arch: "amd64",
            product: "uefi-x64-dev",
            max_bytes: 1024 * 1024,
        };
        fs::create_dir_all(acq.objects()).unwrap();
        for pointer in strings(&case["present"]) {
            let bytes = object(pointer);
            fs::write(acq.objects().join(sha256(&bytes)), bytes).unwrap();
        }
        let result = acq.import(&mut Cursor::new(archive(
            &envelope,
            &strings(&case["archive"]),
        )));
        let name = &case["name"];
        match case["result"].as_str().unwrap() {
            "accepted" => {
                let ready = result.unwrap_or_else(|e| panic!("{name}: {e:#}"));
                for pointer in OBJECTS {
                    assert!(
                        acq.objects().join(sha256(&object(pointer))).is_file(),
                        "{name}"
                    );
                }
                assert!(ready.path.is_file(), "{name}");
            }
            _ => {
                let error = format!("{:#}", result.expect_err(name.as_str().unwrap()));
                if let Some(refusal) = case["refusal"].as_str() {
                    assert!(error.contains(refusal), "{name}: {error}");
                }
                assert!(
                    !dir.path()
                        .join("updates/verified")
                        .read_dir()
                        .unwrap()
                        .any(|e| e.unwrap().path().extension().is_some_and(|x| x == "json")),
                    "{name}: a refused import published a descriptor"
                );
            }
        }
    }
}

/// The BOARD POLICIES of `cases.json`: the `board` section each concrete
/// board's signed boot policy carries, as the assembly writes it. The assembly
/// keeps these same fixture bytes, so a board whose facts change on one side
/// and not the other fails a gate instead of reaching a device.
///
/// There is no vocabulary of accepted names any more. Which board a device is,
/// is its policy's to say; a deployment or firmware for another board, the
/// retired `x64` and `virt-arm64` included, is refused because it is not the
/// board the policy names (`components.rs`
/// `a_deployment_for_another_board_is_refused_by_the_device`).
#[test]
fn board_policies() {
    let cases = cases();
    let policies = cases["boardPolicies"].as_object().unwrap();
    assert!(!policies.is_empty());
    for (name, entry) in policies {
        let facts: mica_deploy::board::BoardFacts = serde_json::from_value(entry["board"].clone())
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        facts
            .validate(entry["arch"].as_str().unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        // The spelling round-trips, so what the device reads is what the
        // assembly wrote and nothing the reader filled in.
        assert_eq!(
            serde_json::to_value(&facts).unwrap(),
            entry["board"],
            "{name}"
        );
        let mut d: Value = serde_json::from_str(PAYLOAD).unwrap();
        d["board"] = json!(name);
        d["arch"] = entry["arch"].clone();
        d["kernel"]["board"] = json!(name);
        d["kernel"]["arch"] = entry["arch"].clone();
        d["kernel"]["boot"]["format"] = json!(facts.kernel.as_str());
        d["rootfs"]["arch"] = entry["arch"].clone();
        d["kernel"]["id"] = component_id(&d["kernel"]).unwrap().into();
        d["rootfs"]["id"] = component_id(&d["rootfs"]).unwrap().into();
        let parsed = mica_deploy::components::parse_deployment(
            serde_json::to_string(&d).unwrap().as_bytes(),
        )
        .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let identity = mica_deploy::components::BootIdentity {
            board: name.clone(),
            arch: entry["arch"].as_str().unwrap().to_owned(),
            kernel_build_id: String::new(),
            kernel_release: String::new(),
            support_id: String::new(),
        };
        mica_deploy::components::admit(&parsed, &identity, facts.kernel)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
    }
}
