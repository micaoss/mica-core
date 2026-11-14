//! The signed boot policy as both of its readers take it: the runkit at boot
//! and the running system's `mica-deploy`, one type.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/boards.rs"]
mod boards;

use mica_deploy::boot::policy::BootPolicy;
use serde_json::{Value, json};

const CASES: &str = include_str!("component-contracts/cases.json");

/// A whole boot policy for `board`, as the assembly writes it.
fn policy(board: &str) -> Value {
    json!({
        "identity": boards::identity(board),
        "board": boards::policy(board),
        "publicKeys": ["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="],
        "systemPartUuid": "6b3b3f4e-6c1d-4c3e-9b55-0a8f4a8e6a11",
        "dataPartUuid": "0c6a1d2e-3f4b-4a5c-8d6e-7f8091a2b3c4",
    })
}

#[test]
fn a_policy_with_data_encryption_reads_as_one_without() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let plain: BootPolicy = serde_json::from_value(policy("uefi-x64")).unwrap();
    assert!(plain.data_encryption.is_none());
    for encryption in cases["dataEncryption"]["accepted"].as_array().unwrap() {
        let mut encrypted = policy("uefi-x64");
        encrypted["dataEncryption"] = encryption.clone();
        // Bounded as the readers bound it.
        assert!(encrypted.to_string().len() <= 4096);
        let read: BootPolicy = serde_json::from_value(encrypted).unwrap();
        read.data_encryption.unwrap().validate().unwrap();
    }
}

#[test]
fn a_field_outside_the_policy_is_refused() {
    let mut policy = policy("uefi-x64");
    policy["dataEncrypton"] = json!({});
    assert!(serde_json::from_value::<BootPolicy>(policy).is_err());
}
