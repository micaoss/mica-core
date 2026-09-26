#[path = "support/boards.rs"]
mod boards;

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::board::KernelFormat;
use mica_deploy::components::{
    BootIdentity, admit, component_id, parse_deployment, verify_deployment,
};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{Value, json};

const PAYLOAD: &str = include_str!("component-contracts/deployment.json");
const CASES: &str = include_str!("component-contracts/cases.json");
const ENVELOPE: &str = include_str!("component-contracts/envelope.json");

fn context() -> BootIdentity {
    serde_json::from_value(serde_json::from_str::<Value>(CASES).unwrap()["context"].clone())
        .unwrap()
}

/// The kernel format the context's board runs, from its signed policy.
fn kernel() -> KernelFormat {
    boards::policy(&context().board).kernel
}

#[test]
fn shared_golden_identity_and_paths() {
    let descriptor = parse_deployment(PAYLOAD.as_bytes()).unwrap();
    let cases: Value = serde_json::from_str(CASES).unwrap();
    assert_eq!(
        component_id(&serde_json::to_value(&descriptor).unwrap()).unwrap(),
        cases["deploymentId"]
    );
    assert_eq!(
        descriptor.paths().unwrap().rootfs,
        format!("roots/{}/rootfs.img", descriptor.rootfs.id)
    );
    assert_eq!(
        descriptor.paths().unwrap().support,
        format!("kernels/{}/support.img", descriptor.kernel.id)
    );
    assert_eq!(
        descriptor.paths().unwrap().boot,
        format!("EFI/mica/kernels/{}.efi", descriptor.kernel.id)
    );
}

#[test]
fn board_formats_tree_levels_and_object_integrity() {
    for board in boards::names() {
        for (blocks, tree_blocks) in [(1, 0), (128, 1), (129, 3), (16385, 132)] {
            let mut v: Value = serde_json::from_str(PAYLOAD).unwrap();
            let arch = boards::arch(&board);
            let format = boards::policy(&board).kernel.as_str();
            v["board"] = board.clone().into();
            v["arch"] = arch.clone().into();
            v["kernel"]["board"] = board.clone().into();
            v["kernel"]["arch"] = arch.clone().into();
            v["rootfs"]["arch"] = arch.clone().into();
            v["kernel"]["boot"]["format"] = format.into();
            v["rootfs"]["content"]["verity"]["dataBlocks"] = blocks.into();
            v["rootfs"]["content"]["verity"]["hashOffset"] = (blocks * 4096).into();
            v["rootfs"]["content"]["image"]["bytes"] = ((blocks + tree_blocks) * 4096).into();
            v["kernel"]["id"] = component_id(&v["kernel"]).unwrap().into();
            v["rootfs"]["id"] = component_id(&v["rootfs"]).unwrap().into();
            let d = parse_deployment(&serde_json::to_vec(&v).unwrap()).unwrap();
            assert!(d.paths().unwrap().boot.ends_with(if format == "uki" {
                ".efi"
            } else {
                "/boot.itb"
            }));
        }
    }
    let bytes = b"component";
    let mut artifact = mica_deploy::components::Artifact {
        bytes: bytes.len() as u64,
        sha256: hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes)),
    };
    assert!(artifact.verify(bytes).is_ok());
    assert!(artifact.verify(b"Component").is_err());
    artifact.bytes += 1;
    assert!(artifact.verify(bytes).is_err());
}

#[test]
fn all_shared_negative_fixtures_fail() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    for case in cases["invalid"].as_array().unwrap() {
        let mut value = cases["valid"].clone();
        let pointer = case["pointer"].as_str().unwrap();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), case["value"].clone());
        // The device reader: the descriptor's own shape, then the running
        // board's policy. Each case names the rule that fires first.
        let name = case["name"].as_str().unwrap();
        let error = match parse_deployment(&serde_json::to_vec(&value).unwrap()) {
            Ok(d) => admit(&d, &context(), kernel()).expect_err(name).to_string(),
            Err(error) => error.to_string(),
        };
        let refusal = case["refusal"].as_str().unwrap();
        assert!(
            error.contains(refusal),
            "{name}: expected {refusal:?}, got {error:?}"
        );
    }
}

/// A deployment well formed for another board passes the parser -- only the
/// device knows it is not the board it runs -- and every device refuses every
/// other board's, by its policy alone.
#[test]
fn a_deployment_for_another_board_is_refused_by_the_device() {
    for built_for in boards::names() {
        let mut v: Value = serde_json::from_str(PAYLOAD).unwrap();
        let arch = boards::arch(&built_for);
        v["board"] = built_for.clone().into();
        v["arch"] = arch.clone().into();
        v["kernel"]["board"] = built_for.clone().into();
        v["kernel"]["arch"] = arch.clone().into();
        v["rootfs"]["arch"] = arch.into();
        v["kernel"]["boot"]["format"] = boards::policy(&built_for).kernel.as_str().into();
        v["kernel"]["id"] = component_id(&v["kernel"]).unwrap().into();
        v["rootfs"]["id"] = component_id(&v["rootfs"]).unwrap().into();
        let d = parse_deployment(&serde_json::to_vec(&v).unwrap()).unwrap();
        for device in boards::names() {
            let result = admit(
                &d,
                &boards::identity(&device),
                boards::policy(&device).kernel,
            );
            if device == built_for {
                result.unwrap();
            } else {
                let error = result.expect_err(&device).to_string();
                assert!(
                    error.contains("board/architecture mismatch"),
                    "{built_for} on {device}: {error}"
                );
            }
        }
        // The right board with the other kernel format is refused too.
        let other = match boards::policy(&built_for).kernel {
            KernelFormat::Uki => KernelFormat::Fit,
            KernelFormat::Fit => KernelFormat::Uki,
        };
        let error = admit(&d, &boards::identity(&built_for), other)
            .expect_err("the other format")
            .to_string();
        assert!(error.contains("wrong boot format"), "{error}");
    }
}

#[test]
fn noncanonical_duplicate_and_oversized_payloads_fail() {
    assert!(
        parse_deployment(
            PAYLOAD
                .replace("\"generation\":1", "\"generation\":2,\"generation\":1")
                .as_bytes()
        )
        .is_err()
    );
    assert!(parse_deployment(format!("{PAYLOAD}\n").as_bytes()).is_err());
    assert!(parse_deployment(&vec![b' '; 16385]).is_err());
}

#[test]
fn existing_server_signature_verifies_and_is_bound_to_running_kernel() {
    let golden: Value = serde_json::from_str(ENVELOPE).unwrap();
    let key = STANDARD
        .decode(golden["publicKey"].as_str().unwrap())
        .unwrap();
    let key: [u8; 32] = key.try_into().unwrap();
    let envelope = ordered_envelope(&golden["envelope"]);
    let descriptor = verify_deployment(&envelope, &[key], &context(), kernel()).unwrap();
    assert_eq!(descriptor.kernel.build_id, context().kernel_build_id);
    let mut wrong = context();
    wrong.support_id = "0".repeat(64);
    assert!(verify_deployment(&envelope, &[key], &wrong, kernel()).is_err());
    assert!(verify_deployment(&envelope, &[[0; 32]], &context(), kernel()).is_err());
}

fn ordered_envelope(e: &Value) -> Vec<u8> {
    format!(
        "{{\"schema\":{},\"keyId\":{},\"payload\":{},\"signature\":{}}}",
        e["schema"], e["keyId"], e["payload"], e["signature"]
    )
    .into_bytes()
}

#[test]
fn signed_schema_substitution_and_tampering_fail() {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let signer = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let key: [u8; 32] = signer.public_key().as_ref().try_into().unwrap();
    let mut e = json!({
        "schema":"mica/update-envelope/v1",
        "keyId":hex::encode(ring::digest::digest(&ring::digest::SHA256, &key)),
        "payload":STANDARD.encode(PAYLOAD),
        "signature":STANDARD.encode(signer.sign(PAYLOAD.as_bytes()).as_ref())
    });
    assert!(verify_deployment(&ordered_envelope(&e), &[key], &context(), kernel()).is_ok());
    let mut changed: Value = serde_json::from_str(PAYLOAD).unwrap();
    changed["kernel"]["support"]["rootHash"] = "3".repeat(64).into();
    changed["kernel"]["id"] = component_id(&changed["kernel"]).unwrap().into();
    let changed = serde_json::to_vec(&changed).unwrap();
    e["payload"] = STANDARD.encode(&changed).into();
    e["signature"] = STANDARD.encode(signer.sign(&changed).as_ref()).into();
    assert!(verify_deployment(&ordered_envelope(&e), &[key], &context(), kernel()).is_err());
    e["signature"] = STANDARD.encode([0; 64]).into();
    assert!(verify_deployment(&ordered_envelope(&e), &[key], &context(), kernel()).is_err());
    let substitute = b"{\"schema\":\"mica/update-catalog/v1\"}";
    e["payload"] = STANDARD.encode(substitute).into();
    e["signature"] = STANDARD.encode(signer.sign(substitute).as_ref()).into();
    assert!(verify_deployment(&ordered_envelope(&e), &[key], &context(), kernel()).is_err());
}
