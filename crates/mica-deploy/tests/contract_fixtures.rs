//! The committed component-contract fixtures are exactly what the generator
//! makes of them, with the documented test-only keys.

#[path = "support/contract_fixtures.rs"]
mod contract_fixtures;

use base64::{Engine, engine::general_purpose::STANDARD};
use contract_fixtures::{DEPLOYMENT_KEY_LABEL, FIRMWARE_KEY_LABEL, generate, key};
use ring::signature::KeyPair;
use serde_json::Value;

const DEPLOYMENT: &str = include_str!("component-contracts/deployment.json");
const ENVELOPE: &str = include_str!("component-contracts/envelope.json");
const FIRMWARE: &str = include_str!("component-contracts/firmware.json");
const CASES: &str = include_str!("component-contracts/cases.json");
const CATALOG: &str = include_str!("component-contracts/catalog.json");
const CHUNKER: &str = include_str!("component-contracts/chunker.json");

#[test]
fn committed_fixtures_are_the_generators_fixed_point() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let firmware: Value = serde_json::from_str(FIRMWARE).unwrap();
    let generated = generate(&cases, &firmware);
    assert_eq!(generated.deployment, DEPLOYMENT, "deployment.json");
    assert_eq!(generated.envelope, ENVELOPE, "envelope.json");
    assert_eq!(generated.firmware, FIRMWARE, "firmware.json");
    assert_eq!(generated.cases, CASES, "cases.json");
    assert_eq!(generated.catalog, CATALOG, "catalog.json");
    assert_eq!(generated.chunker, CHUNKER, "chunker.json");
    // Regenerating from the generated inputs changes nothing.
    let again = generate(
        &serde_json::from_str(&generated.cases).unwrap(),
        &serde_json::from_str(&generated.firmware).unwrap(),
    );
    assert_eq!(again.firmware, generated.firmware);
    assert_eq!(again.cases, generated.cases);
    assert_eq!(again.catalog, generated.catalog);
    assert_eq!(again.chunker, generated.chunker);
}

#[test]
fn fixture_keys_are_the_labelled_test_only_keys() {
    for (file, label) in [
        (ENVELOPE, DEPLOYMENT_KEY_LABEL),
        (FIRMWARE, FIRMWARE_KEY_LABEL),
    ] {
        let value: Value = serde_json::from_str(file).unwrap();
        assert!(label.contains("TEST-ONLY"));
        assert_eq!(
            value["publicKey"],
            STANDARD.encode(key(label).public_key().as_ref())
        );
    }
}
