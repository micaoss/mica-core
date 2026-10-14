//! `mica/set/v1` of kind `core`: the core components of a channel, released once for every product of an
//! architecture, and the selection a device makes from it.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::components::{component_id, parse_deployment};
use mica_deploy::core_set::{self, device_features};
use mica_deploy::update_set::{self, UpdateSet};
use serde_json::{Value, json};

const VECTOR: &str = include_str!("component-contracts/set.json");
const DEPLOYMENT: &str = include_str!("component-contracts/deployment.json");

fn vector() -> Value {
    serde_json::from_str(VECTOR).unwrap()
}

/// The set's exact signed bytes.
fn payload() -> String {
    vector()["payload"].as_str().unwrap().to_owned()
}

fn golden() -> Value {
    serde_json::from_str(&payload()).unwrap()
}

fn parse(value: &Value) -> Result<UpdateSet, String> {
    update_set::parse(&serde_json::to_vec(value).unwrap()).map_err(|err| err.to_string())
}

/// `edit` applied to the golden set, the components' identities recomputed.
fn edited(edit: impl FnOnce(&mut Value)) -> Value {
    let mut set = golden();
    edit(&mut set);
    for component in set["components"].as_array_mut().unwrap() {
        component["id"] = json!(component_id(component).unwrap());
    }
    set
}

fn names(set: &UpdateSet, features: &[&str], level: u64) -> Result<Vec<String>, String> {
    let features: Vec<String> = features.iter().map(|f| (*f).to_owned()).collect();
    core_set::select(set, &features, level)
        .map(|chosen| chosen.iter().map(|c| c.package.clone()).collect())
        .map_err(|err| err.to_string())
}

#[test]
fn the_golden_set_is_canonical_and_parses() {
    let set = parse(&golden()).unwrap();
    assert_eq!(set.channel, "general");
    assert_eq!(set.arch, "amd64");
    assert_eq!(set.generation, 1);
    let packages: Vec<&str> = set.components.iter().map(|c| c.package.as_str()).collect();
    assert_eq!(packages, ["mica-apid-ui", "micad"]);
    assert_eq!(serde_json::to_string(&golden()).unwrap(), payload());
    assert_eq!(set.id().unwrap(), vector()["setId"]);
}

#[test]
fn a_malformed_set_is_refused_by_name() {
    let cases: Vec<(Value, &str)> = vec![
        (
            edited(|s| s["schema"] = json!("mica/set/v2")),
            "wrong set schema",
        ),
        (
            edited(|s| s["channel"] = json!("General")),
            "invalid set channel",
        ),
        (edited(|s| s["channel"] = json!("")), "invalid set channel"),
        (
            edited(|s| s["kind"] = json!("charts")),
            "unsupported set kind",
        ),
        (
            edited(|s| s["arch"] = json!("riscv64")),
            "unsupported architecture",
        ),
        (edited(|s| s["generation"] = json!(0)), "invalid integer"),
        (
            edited(|s| s["components"] = json!([])),
            "a set carries no component",
        ),
        (
            edited(|s| s["components"].as_array_mut().unwrap().reverse()),
            "set components not unique and sorted by package",
        ),
        (
            edited(|s| s["components"][1]["arch"] = json!("arm64")),
            "component target mismatch",
        ),
        (
            edited(|s| {
                s["components"][0]["needs"][0] =
                    json!({ "max": "0.0.5", "min": "0.0.5", "package": "micad" })
            }),
            "a core component's need is outside its version range",
        ),
        (
            edited(|s| s["components"][0]["needs"][0]["package"] = json!("mica-mqttd")),
            "a core component's need is not in the set",
        ),
    ];
    for (value, refusal) in cases {
        let err = parse(&value).unwrap_err();
        assert!(err.contains(refusal), "{err} does not name {refusal}");
    }
    let mut stale = golden();
    stale["components"][0]["version"] = json!("0.0.5");
    assert!(
        parse(&stale)
            .unwrap_err()
            .contains("component identity mismatch")
    );
}

/// The rules a component is held to wherever it is read, each by its name.
#[test]
fn a_component_that_does_not_fit_is_refused_by_name() {
    let cases: Vec<(Value, &str)> = vec![
        (
            edited(|s| s["components"][0]["needs"][0]["package"] = json!("mica-apid-ui")),
            "a core component needs itself",
        ),
        (
            edited(|s| s["components"][1]["features"] = json!([])),
            "invalid core features",
        ),
        (
            edited(|s| {
                s["components"][1]["needs"] = json!(
                    (0..17)
                        .map(|n| json!({ "min": "1", "package": format!("p{n}") }))
                        .collect::<Vec<_>>()
                );
            }),
            "invalid core needs",
        ),
        (
            edited(|s| {
                s["components"][0]["needs"][0] =
                    json!({ "max": "0.0.3", "min": "0.0.4", "package": "micad" })
            }),
            "empty version range",
        ),
        (
            edited(|s| s["components"][1]["root"] = json!({ "max": 1, "min": 2 })),
            "empty root interface range",
        ),
        (
            edited(|s| s["components"][1]["version"] = json!("1.x")),
            "invalid version",
        ),
    ];
    for (value, refusal) in cases {
        let err = parse(&value).unwrap_err();
        assert!(err.contains(refusal), "{err} does not name {refusal}");
    }
}

/// THE SELECTION RULE, AS DATA: every case of the shared vector, which the
/// producer's port of the rule is held to as well.
#[test]
fn the_selection_vector_holds() {
    let set = parse(&golden()).unwrap();
    let vector = vector();
    let cases = vector["selections"].as_array().unwrap();
    assert!(cases.len() >= 6);
    for case in cases {
        let features: Vec<&str> = case["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        let result = names(&set, &features, case["rootLevel"].as_u64().unwrap());
        match case["refusal"].as_str() {
            Some(refusal) => {
                let err = result.expect_err(case["name"].as_str().unwrap());
                assert!(err.contains(refusal), "{}: {err}", case["name"]);
            }
            None => {
                let selected: Vec<String> = case["selected"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(result.unwrap(), selected, "{}", case["name"]);
            }
        }
    }
}

/// A need met by a component outside its range is refused where the set is
/// read, before any product selects from it.
#[test]
fn a_selection_follows_the_roots_level() {
    let bounded = parse(&edited(|s| {
        s["components"][1]["root"] = json!({ "min": 2 })
    }))
    .unwrap();
    assert!(names(&bounded, &["micad"], 1).is_err());
    assert_eq!(names(&bounded, &["micad"], 2).unwrap(), ["micad"]);
}

#[test]
fn the_features_are_read_from_the_product_file() {
    let text = "PRODUCT=uefi-x64.full\nBOARD=uefi-x64\nFEATURES=\"micad ui ssh tools mqtt containers\"\nCOMPONENTS=\"\"\n";
    assert_eq!(
        device_features(text).unwrap(),
        ["micad", "ui", "ssh", "tools", "mqtt", "containers"]
    );
    assert_eq!(
        device_features("PRODUCT=x.y\nFEATURES=\"\"\n").unwrap(),
        Vec::<String>::new()
    );
    assert!(device_features("PRODUCT=x.y\n").is_err());
    assert!(device_features("FEATURES=\"a\"\nFEATURES=\"b\"\n").is_err());
    assert!(device_features("FEATURES=\"Bad!\"\n").is_err());
}

#[test]
fn the_signed_vector_authenticates_and_another_key_does_not() {
    let vector = vector();
    let public: [u8; 32] = STANDARD
        .decode(vector["publicKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let envelope = vector["envelope"].as_str().unwrap().as_bytes();
    assert_eq!(
        update_set::authenticate(envelope, &[public])
            .unwrap()
            .generation,
        1
    );
    assert!(update_set::authenticate(envelope, &[[7u8; 32]]).is_err());
    // An envelope around anything else is not a core set.
    let key = Ed25519KeyPair::from_seed_unchecked(&[5; 32]).unwrap();
    let other: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    let bytes = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hex::encode(digest::digest(&digest::SHA256, &other)),
        STANDARD.encode(DEPLOYMENT),
        STANDARD.encode(key.sign(DEPLOYMENT.as_bytes()).as_ref())
    );
    assert!(update_set::authenticate(bytes.as_bytes(), &[other]).is_err());
}

/// A deployment is the system alone: one that names core components is not
/// read.
#[test]
fn a_deployment_names_no_core_components() {
    let mut d: Value = serde_json::from_str(DEPLOYMENT).unwrap();
    assert_eq!(d["schema"], "mica/deployment/v1");
    assert!(parse_deployment(DEPLOYMENT.as_bytes()).is_ok());
    let set = golden();
    d["core"] = json!([set["components"][1]]);
    assert!(parse_deployment(&serde_json::to_vec(&d).unwrap()).is_err());
}
