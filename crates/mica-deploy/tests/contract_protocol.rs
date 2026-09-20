//! The protocol as data: the shared catalog vector and the schema strings of
//! `component-contracts/cases.json`, driven through the readers.
//!
//! The vector is what an online update looks like on the wire -- one signed
//! `mica/catalog/v2` document serving the golden deployment from a real source
//! URL. A server implementation has bytes to verify itself against rather than
//! a schema string it spells from memory, which is what let a second update
//! server be written on the superseded protocol without anything failing.

// The generator module is shared with `contract_fixtures.rs` and the
// regeneration example; this test uses its keys and signer, not its writer.
#[allow(dead_code)]
#[path = "support/contract_fixtures.rs"]
mod contract_fixtures;

use base64::{Engine, engine::general_purpose::STANDARD};
use contract_fixtures::{DEPLOYMENT_KEY_LABEL, key, sign};
use mica_deploy::{
    catalog::{CatalogRequest, verify_catalog},
    components::{authenticate_deployment, parse_deployment},
};
use ring::signature::KeyPair;
use serde_json::{Value, json};

const CASES: &str = include_str!("component-contracts/cases.json");
const CATALOG: &str = include_str!("component-contracts/catalog.json");
const PAYLOAD: &str = include_str!("component-contracts/deployment.json");

fn cases() -> Value {
    serde_json::from_str(CASES).unwrap()
}
fn vector() -> Value {
    serde_json::from_str(CATALOG).unwrap()
}
fn trust() -> [u8; 32] {
    STANDARD
        .decode(vector()["publicKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}
/// An envelope exactly as the reader requires it: its four fields in the
/// declared order, which is not the order a `Value` re-serializes them in.
fn envelope(e: &Value) -> Vec<u8> {
    format!(
        "{{\"schema\":{},\"keyId\":{},\"payload\":{},\"signature\":{}}}",
        e["schema"], e["keyId"], e["payload"], e["signature"]
    )
    .into_bytes()
}
/// Re-sign a mutated payload with the vector's own test-only key.
fn resign(payload: &Value) -> Vec<u8> {
    sign(
        &key(DEPLOYMENT_KEY_LABEL),
        &serde_json::to_vec(payload).unwrap(),
    )
    .into_bytes()
}
fn payload_of(vector: &Value) -> Value {
    serde_json::from_slice(
        &STANDARD
            .decode(vector["envelope"]["payload"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}
fn request<'a>(
    source: &'a str,
    cases: &'a Value,
    channel: &'a str,
    now: i64,
) -> CatalogRequest<'a> {
    CatalogRequest {
        source,
        board: cases["valid"]["board"].as_str().unwrap(),
        arch: cases["valid"]["arch"].as_str().unwrap(),
        product: cases["valid"]["product"].as_str().unwrap(),
        channel,
        now,
        checkpoint: None,
        highest_generation: 0,
    }
}

/// The shared vector authenticates, and it selects exactly the golden release.
#[test]
fn the_catalog_vector_is_served_and_selected() {
    let vector = vector();
    let cases = cases();
    let source = vector["source"].as_str().unwrap();
    let now = vector["now"].as_i64().unwrap();
    assert_eq!(
        vector["publicKey"],
        STANDARD.encode(key(DEPLOYMENT_KEY_LABEL).public_key().as_ref()),
        "the vector is signed by the documented test-only key"
    );

    let verified = verify_catalog(
        &envelope(&vector["envelope"]),
        &[trust()],
        &request(source, &cases, "stable", now),
    )
    .expect("the shared catalog vector must verify");

    let selected = verified.selected.expect("a release for this device");
    assert_eq!(selected.deployment_id, cases["deploymentId"]);
    let shared: Value =
        serde_json::from_str(include_str!("component-contracts/envelope.json")).unwrap();
    assert_eq!(
        selected.envelope.as_bytes(),
        envelope(&shared["envelope"]),
        "the release carries the shared deployment envelope verbatim"
    );
    assert_eq!(
        verified.checkpoint.revision,
        payload_of(&vector)["revision"]
    );
    for object in &selected.objects {
        assert_eq!(
            object.url,
            format!(
                "{}/v1/objects/{}",
                source.strip_suffix("/v1/manifest.json").unwrap(),
                object.sha256
            ),
            "objects are served from the catalog's own origin, by digest"
        );
    }
}

/// A device of another product is offered nothing from the same vector.
#[test]
fn the_vector_selects_nothing_for_another_product() {
    let vector = vector();
    let mut cases = cases();
    cases["valid"]["product"] = json!("uefi-x64-prod");
    let verified = verify_catalog(
        &envelope(&vector["envelope"]),
        &[trust()],
        &request(
            vector["source"].as_str().unwrap(),
            &cases,
            "stable",
            vector["now"].as_i64().unwrap(),
        ),
    )
    .expect("the catalog still verifies");
    assert!(verified.selected.is_none());
}

/// THE PROTOCOL VOCABULARY. The accepted strings are the ones the shared
/// fixtures carry; every retired or unknown spelling is refused, with no
/// aliases and no transition. `mica/catalog/v1`, `mica/deployment/v1` and
/// `mica/rootfs/v1` are listed because a second update server implements them
/// today; a reader that quietly took them would hide that.
#[test]
fn schema_vocabulary() {
    let cases = cases();
    let accepted = &cases["schemas"]["accepted"];
    let refused = &cases["schemas"]["refused"];
    let descriptor: Value = serde_json::from_str(PAYLOAD).unwrap();
    let vector = vector();

    // Accepted: exactly what the fixtures are built from.
    assert_eq!(descriptor["schema"], accepted["deployment"]);
    assert_eq!(descriptor["kernel"]["schema"], accepted["kernel"]);
    assert_eq!(descriptor["rootfs"]["schema"], accepted["rootfs"]);
    assert_eq!(vector["envelope"]["schema"], accepted["envelope"]);
    assert_eq!(payload_of(&vector)["schema"], accepted["catalog"]);
    parse_deployment(PAYLOAD.as_bytes()).expect("the accepted deployment schema parses");

    // Refused: the deployment, kernel and rootfs strings, inside the descriptor.
    for (pointer, kind) in [
        ("/schema", "deployment"),
        ("/kernel/schema", "kernel"),
        ("/rootfs/schema", "rootfs"),
    ] {
        for spelling in refused[kind].as_array().unwrap() {
            let mut d = descriptor.clone();
            *d.pointer_mut(pointer).unwrap() = spelling.clone();
            assert!(
                parse_deployment(serde_json::to_string(&d).unwrap().as_bytes()).is_err(),
                "{pointer} accepted {spelling}"
            );
        }
    }

    // Refused: the envelope string, re-signed so only the schema is at fault.
    for spelling in refused["envelope"].as_array().unwrap() {
        let mut e: Value = vector["envelope"].clone();
        e["schema"] = spelling.clone();
        assert!(
            authenticate_deployment(&envelope(&e), &[trust()]).is_err(),
            "the envelope accepted {spelling}"
        );
    }

    // Refused: the catalog string, re-signed with the vector's own key so the
    // signature is valid and the schema is the only thing wrong.
    for spelling in refused["catalog"].as_array().unwrap() {
        let mut p = payload_of(&vector);
        p["schema"] = spelling.clone();
        assert!(
            verify_catalog(
                &resign(&p),
                &[trust()],
                &request(
                    vector["source"].as_str().unwrap(),
                    &cases,
                    "stable",
                    vector["now"].as_i64().unwrap()
                ),
            )
            .is_err(),
            "the catalog accepted {spelling}"
        );
    }
}
