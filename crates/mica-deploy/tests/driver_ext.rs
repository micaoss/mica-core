//! The driver extension vector (`component-contracts/driver-ext.json`): its
//! valid pair is accepted for the golden deployment's kernel, each of its cases
//! draws the refusal it names, and together with the cases below they fire
//! every refusal `src/driver_ext.rs` has.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mica_deploy::components::parse_deployment;
use mica_deploy::driver_ext::{MAX_DESCRIPTOR_BYTES, MAX_MODULES, check};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const VECTOR: &str = include_str!("component-contracts/driver-ext.json");
const DEPLOYMENT: &str = include_str!("component-contracts/deployment.json");
const SOURCE: &str = include_str!("../src/driver_ext.rs");

fn vector() -> Value {
    serde_json::from_str(VECTOR).unwrap()
}

/// The refusal `check` draws for the pair, or `None` when it accepts it.
fn refusal(descriptor: &[u8], manifest: &[u8]) -> Option<String> {
    let kernel = parse_deployment(DEPLOYMENT.as_bytes()).unwrap().kernel;
    check(descriptor, manifest, &kernel)
        .err()
        .map(|error| format!("{error:#}"))
}

#[test]
fn the_valid_pair_is_accepted_for_the_golden_kernel() {
    let vector = vector();
    let kernel = parse_deployment(DEPLOYMENT.as_bytes()).unwrap().kernel;
    assert_eq!(vector["kernel"]["id"], kernel.id.as_str());
    assert_eq!(vector["kernel"]["release"], kernel.release.as_str());
    let descriptor = vector["descriptorBytes"].as_str().unwrap();
    let manifest = vector["manifestBytes"].as_str().unwrap();
    assert_eq!(
        descriptor,
        serde_json::to_string(&vector["descriptor"]).unwrap()
    );
    assert_eq!(
        manifest,
        serde_json::to_string(&vector["manifest"]).unwrap()
    );
    let (extension, manifest) = check(descriptor.as_bytes(), manifest.as_bytes(), &kernel).unwrap();
    assert_eq!(extension.id, vector["extId"].as_str().unwrap());
    assert_eq!(
        manifest.module_directory(),
        format!("modules/{}/updates/mica-ext/rtl8125", kernel.release)
    );
}

/// Apply a case's changes to the valid pair and serialize it.
fn apply(vector: &Value, changes: &[Value]) -> (Vec<u8>, Vec<u8>) {
    let mut documents = [vector["descriptor"].clone(), vector["manifest"].clone()];
    let mut bytes: [Option<Vec<u8>>; 2] = [None, None];
    for change in changes {
        let index = match change["document"].as_str().unwrap() {
            "descriptor" => 0,
            "manifest" => 1,
            other => panic!("unknown document {other}"),
        };
        if let Some(raw) = change["bytes"].as_str() {
            bytes[index] = Some(raw.as_bytes().to_vec());
        } else if change["pretty"] == true {
            bytes[index] = Some(serde_json::to_vec_pretty(&documents[index]).unwrap());
        } else {
            let pointer = change["pointer"].as_str().unwrap();
            let (parent, field) = pointer.rsplit_once('/').unwrap();
            let target = documents[index]
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap();
            if change["remove"] == true {
                assert!(target.remove(field).is_some(), "{pointer}");
            } else {
                target.insert(field.to_owned(), change["value"].clone());
            }
        }
    }
    let [descriptor, manifest] = documents;
    let [descriptor_bytes, manifest_bytes] = bytes;
    (
        descriptor_bytes.unwrap_or_else(|| serde_json::to_vec(&descriptor).unwrap()),
        manifest_bytes.unwrap_or_else(|| serde_json::to_vec(&manifest).unwrap()),
    )
}

/// The cases a fixture would hold only as bulk: bounds.
fn bounds(vector: &Value) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let long = "1".repeat(MAX_DESCRIPTOR_BYTES);
    let (descriptor, _) = apply(
        vector,
        &[json!({"document": "descriptor", "pointer": "/version", "value": long})],
    );
    let (_, many) = apply(
        vector,
        &[
            json!({"document": "manifest", "pointer": "/load", "value": vec!["r8125"; MAX_MODULES + 1]}),
        ],
    );
    let manifest = vector["manifestBytes"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    let descriptor_bytes = vector["descriptorBytes"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    vec![
        ("driver extension document too large", descriptor, manifest),
        ("too many driver extension entries", descriptor_bytes, many),
    ]
}

#[test]
fn every_case_draws_the_refusal_it_names() {
    let vector = vector();
    let mut names = BTreeSet::new();
    for case in vector["invalid"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert!(names.insert(name), "{name} twice");
        let (descriptor, manifest) = apply(&vector, case["changes"].as_array().unwrap());
        let error = refusal(&descriptor, &manifest).unwrap_or_else(|| panic!("{name} accepted"));
        let expected = case["refusal"].as_str().unwrap();
        assert!(error.contains(expected), "{name}: {error}");
    }
    for (expected, descriptor, manifest) in bounds(&vector) {
        let error = refusal(&descriptor, &manifest).expect(expected);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

/// The refusals `src/driver_ext.rs` spells: the message of every `ensure!`
/// and `anyhow!`.
fn source_refusals() -> BTreeSet<String> {
    let mut refusals = BTreeSet::new();
    // An `ensure!` ends its call with its message.
    for (start, call) in SOURCE.match_indices("ensure!(") {
        let rest = &SOURCE[start + call.len()..];
        let call = &rest[..rest.find(");").unwrap()];
        let end = call.rfind('"').unwrap();
        let begin = call[..end].rfind('"').unwrap();
        refusals.insert(call[begin + 1..end].to_owned());
    }
    // An `anyhow!` starts with it.
    for (start, call) in SOURCE.match_indices("anyhow!(\"") {
        let rest = &SOURCE[start + call.len()..];
        refusals.insert(rest[..rest.find('"').unwrap()].to_owned());
    }
    refusals
}

#[test]
fn every_refusal_of_the_reader_is_exercised() {
    let vector = vector();
    let mut exercised: BTreeSet<String> = vector["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["refusal"].as_str().unwrap().to_owned())
        .collect();
    exercised.extend(
        bounds(&vector)
            .into_iter()
            .map(|(refusal, _, _)| refusal.to_owned()),
    );
    let missing: Vec<String> = source_refusals()
        .into_iter()
        .filter(|refusal| !exercised.contains(refusal))
        .collect();
    assert!(missing.is_empty(), "never fired: {missing:?}");
}
