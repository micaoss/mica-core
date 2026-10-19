//! `armhf` is an architecture of every v1 document: a board policy, a
//! deployment and its components, a core set and its components, firmware.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/boards.rs"]
mod boards;

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    board::BoardFacts,
    components::{component_id, parse_deployment},
    firmware::parse_firmware,
    update_set,
};
use serde_json::{Value, json};

/// A payload as its reader takes it: canonical JSON.
fn canonical(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn an_armhf_board_policy_validates() {
    let facts: BoardFacts =
        serde_json::from_value(serde_json::to_value(boards::policy("cx3576")).unwrap()).unwrap();
    facts.validate("armhf").unwrap();
}

#[test]
fn an_armhf_deployment_parses() {
    let mut deployment: Value =
        serde_json::from_str(include_str!("component-contracts/deployment.json")).unwrap();
    deployment["arch"] = json!("armhf");
    for component in ["kernel", "rootfs"] {
        deployment[component]["arch"] = json!("armhf");
        let id = component_id(&deployment[component]).unwrap();
        deployment[component]["id"] = json!(id);
    }
    assert_eq!(
        parse_deployment(&canonical(&deployment)).unwrap().arch,
        "armhf"
    );
}

#[test]
fn an_armhf_core_set_parses() {
    let vector: Value = serde_json::from_str(include_str!("component-contracts/set.json")).unwrap();
    let mut set: Value = serde_json::from_str(vector["payload"].as_str().unwrap()).unwrap();
    set["arch"] = json!("armhf");
    for component in set["components"].as_array_mut().unwrap() {
        component["arch"] = json!("armhf");
        component["id"] = json!(component_id(component).unwrap());
    }
    assert_eq!(update_set::parse(&canonical(&set)).unwrap().arch, "armhf");
}

#[test]
fn an_armhf_firmware_manifest_parses() {
    let fixture: Value =
        serde_json::from_str(include_str!("component-contracts/firmware.json")).unwrap();
    let envelope: Value =
        serde_json::from_str(fixture["records"][0]["envelope"].as_str().unwrap()).unwrap();
    let payload = STANDARD
        .decode(envelope["payload"].as_str().unwrap())
        .unwrap();
    let mut firmware: Value = serde_json::from_slice(&payload).unwrap();
    firmware["arch"] = json!("armhf");
    firmware["id"] = json!(component_id(&firmware).unwrap());
    assert_eq!(parse_firmware(&canonical(&firmware)).unwrap().arch, "armhf");
}
