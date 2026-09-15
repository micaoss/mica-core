//! The component contract accepts only its exact schema ids: any other spelling
//! or version is refused, even when the record is otherwise valid and correctly
//! signed. There is no alias and no fallback.

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::components::{authenticate_deployment, component_id, parse_deployment};
use mica_deploy::firmware::{authenticate_firmware, parse_firmware};
use ring::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{Value, json};

const PAYLOAD: &str = include_str!("component-contracts/deployment.json");
const FIRMWARE: &str = include_str!("component-contracts/firmware.json");

fn with_ids(mut d: Value) -> Vec<u8> {
    d["kernel"]["id"] = component_id(&d["kernel"]).unwrap().into();
    d["rootfs"]["id"] = component_id(&d["rootfs"]).unwrap().into();
    serde_json::to_vec(&d).unwrap()
}

fn envelope(schema: &str, key: &Ed25519KeyPair, payload: &[u8]) -> Vec<u8> {
    format!(
        "{{\"schema\":\"{schema}\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hex::encode(digest::digest(&digest::SHA256, key.public_key().as_ref())),
        STANDARD.encode(payload),
        STANDARD.encode(key.sign(payload).as_ref())
    )
    .into_bytes()
}

#[test]
fn the_shared_descriptor_uses_the_mica_schemas() {
    let d = parse_deployment(PAYLOAD.as_bytes()).unwrap();
    assert_eq!(d.schema, "mica/deployment/v2");
    assert_eq!(d.kernel.schema, "mica/kernel/v1");
    assert_eq!(d.rootfs.schema, "mica/rootfs/v1");
}

#[test]
fn other_component_schemas_are_refused_with_correct_ids() {
    let valid: Value = serde_json::from_str(PAYLOAD).unwrap();
    assert!(parse_deployment(&with_ids(valid.clone())).is_ok());
    for (pointer, other) in [
        ("/schema", "mica/deployment/v1"),
        ("/kernel/schema", "mica/kernel/v2"),
        ("/rootfs/schema", "mica/rootfs/v2"),
    ] {
        let mut d = valid.clone();
        *d.pointer_mut(pointer).unwrap() = json!(other);
        assert!(parse_deployment(&with_ids(d)).is_err(), "{other}");
    }
}

#[test]
fn a_signed_envelope_under_another_schema_is_refused() {
    let key = Ed25519KeyPair::from_seed_unchecked(&[5; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let payload = PAYLOAD.as_bytes();
    assert!(
        authenticate_deployment(
            &envelope("mica/update-envelope/v1", &key, payload),
            &[public]
        )
        .is_ok()
    );
    assert!(
        authenticate_deployment(
            &envelope("mica/update-envelope/v2", &key, payload),
            &[public]
        )
        .is_err()
    );
}

#[test]
fn firmware_under_another_schema_is_refused() {
    let fixture: Value = serde_json::from_str(FIRMWARE).unwrap();
    let mut manifest = fixture["records"][0]["manifest"].clone();
    assert_eq!(manifest["schema"], "mica/firmware/v1");
    assert!(parse_firmware(&serde_json::to_vec(&manifest).unwrap()).is_ok());
    manifest["schema"] = json!("mica/firmware/v2");
    manifest["id"] = json!(component_id(&manifest).unwrap());
    let payload = serde_json::to_vec(&manifest).unwrap();
    assert!(parse_firmware(&payload).is_err());
    let key = Ed25519KeyPair::from_seed_unchecked(&[6; 32]).unwrap();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    assert!(
        authenticate_firmware(
            &envelope("mica/update-envelope/v1", &key, &payload),
            &[public]
        )
        .is_err()
    );
}
