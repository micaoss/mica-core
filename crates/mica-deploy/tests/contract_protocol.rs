//! The protocol as data: the shared catalog vector and the schema strings of
//! `component-contracts/cases.json`, driven through the readers.
//!
//! The vector is what an online update looks like on the wire -- the unsigned
//! `mica/catalog/v2` manifest, the `mica/release/v1` document it links to and
//! the signed descriptor that links to, serving the golden deployment, every
//! file on another host than the source. A server implementation has bytes to
//! verify itself against rather than a schema string it spells from memory.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The generator module is shared with `contract_fixtures.rs` and the
// regeneration example; this test uses its keys and signer, not its writer.
#[allow(dead_code)]
#[path = "support/contract_fixtures.rs"]
mod contract_fixtures;

use aws_lc_rs::signature::KeyPair;
use base64::{Engine, engine::general_purpose::STANDARD};
use contract_fixtures::{DEPLOYMENT_KEY_LABEL, key};
use mica_deploy::{
    catalog::{
        CatalogRequest, CoreCatalogRequest, VerifiedCatalog, verify_catalog, verify_core_catalog,
    },
    components::{authenticate_deployment, parse_deployment},
    core_set,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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
/// The files the vector serves, by URL, as their exact wire bytes.
fn files(vector: &Value) -> BTreeMap<String, Vec<u8>> {
    [
        ("manifestUrl", "manifest"),
        ("releaseUrl", "release"),
        ("descriptorUrl", "descriptor"),
        ("coreReleaseUrl", "coreRelease"),
        ("coreSetUrl", "coreSet"),
    ]
    .into_iter()
    .map(|(url, body)| {
        (
            vector[url].as_str().unwrap().to_owned(),
            vector[body].as_str().unwrap().as_bytes().to_vec(),
        )
    })
    .collect()
}
fn document_of(vector: &Value, name: &str) -> Value {
    serde_json::from_str(vector[name].as_str().unwrap()).unwrap()
}
fn request<'a>(source: &'a str, cases: &'a Value) -> CatalogRequest<'a> {
    CatalogRequest {
        source,
        board: cases["valid"]["board"].as_str().unwrap(),
        arch: cases["valid"]["arch"].as_str().unwrap(),
        product: cases["valid"]["product"].as_str().unwrap(),
        checkpoint: None,
        highest_generation: 0,
    }
}
/// Verify against `files`, returning the result and the URLs fetched.
fn run(
    files: &BTreeMap<String, Vec<u8>>,
    request: &CatalogRequest<'_>,
) -> (anyhow::Result<VerifiedCatalog>, Vec<String>) {
    let mut fetched = Vec::new();
    let result = verify_catalog(&[trust()], request, |url, _| {
        fetched.push(url.to_string());
        files
            .get(url.as_str())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("404 {url}"))
    });
    (result, fetched)
}

/// The shared vector verifies, it selects exactly the golden release, and the
/// reader fetches exactly the vector's three URLs in order.
#[test]
fn the_catalog_vector_is_served_and_selected() {
    let vector = vector();
    let cases = cases();
    let source = vector["source"].as_str().unwrap();
    assert_eq!(
        vector["publicKey"],
        STANDARD.encode(key(DEPLOYMENT_KEY_LABEL).public_key().as_ref()),
        "the release descriptor is signed by the documented test-only key"
    );

    let (verified, fetched) = run(&files(&vector), &request(source, &cases));
    let verified = verified.expect("the shared catalog vector must verify");
    assert_eq!(
        fetched,
        [
            &vector["manifestUrl"],
            &vector["releaseUrl"],
            &vector["descriptorUrl"]
        ]
        .map(|url| url.as_str().unwrap().to_owned())
    );

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
        document_of(&vector, "manifest")["revision"]
    );
    let base = document_of(&vector, "release")["baseUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    let source_host = url::Url::parse(source)
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned();
    for object in &selected.objects {
        assert_eq!(
            object.url,
            format!("{base}objects/{}", object.sha256),
            "objects are fetched from the release's baseUrl and their path"
        );
        assert_ne!(
            url::Url::parse(&object.url).unwrap().host_str(),
            Some(source_host.as_str()),
            "the vector's objects sit on another host than the source"
        );
    }
}

/// A device of another product, or one already current, reads the manifest
/// and is offered nothing.
#[test]
fn the_vector_selects_nothing_for_another_product_or_a_current_device() {
    let vector = vector();
    let source = vector["source"].as_str().unwrap();
    let mut other = cases();
    other["valid"]["product"] = json!("uefi-x64-prod");
    let current = cases();
    for request in [
        request(source, &other),
        CatalogRequest {
            highest_generation: current["valid"]["generation"].as_u64().unwrap(),
            ..request(source, &current)
        },
    ] {
        let (verified, fetched) = run(&files(&vector), &request);
        assert!(
            verified
                .expect("the catalog still verifies")
                .selected
                .is_none()
        );
        assert_eq!(fetched, [vector["manifestUrl"].as_str().unwrap()]);
    }
}

/// Verify the vector's core line for `channel`, returning the URLs fetched.
fn run_core(
    files: &BTreeMap<String, Vec<u8>>,
    source: &str,
    arch: &str,
    channel: &str,
    highest_generation: u64,
) -> (
    anyhow::Result<mica_deploy::catalog::VerifiedCoreCatalog>,
    Vec<String>,
) {
    let mut fetched = Vec::new();
    let result = verify_core_catalog(
        &[trust()],
        &CoreCatalogRequest {
            source,
            arch,
            channel,
            checkpoint: None,
            highest_generation,
        },
        |url, _| {
            fetched.push(url.to_string());
            files
                .get(url.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("404 {url}"))
        },
    );
    (result, fetched)
}

/// The vector's core line: the same manifest, the core release's document and
/// the signed set, which is the core set vector's own envelope.
#[test]
fn the_core_line_of_the_vector_is_served_and_selected() {
    let vector = vector();
    let cases = cases();
    let source = vector["source"].as_str().unwrap();
    let arch = cases["valid"]["arch"].as_str().unwrap();
    let channel = cases["coreSet"]["channel"].as_str().unwrap();
    let set_vector: Value =
        serde_json::from_str(include_str!("component-contracts/core-set.json")).unwrap();
    assert_eq!(vector["coreSet"], set_vector["envelope"]);

    let (verified, fetched) = run_core(&files(&vector), source, arch, channel, 0);
    let selected = verified
        .expect("the core line must verify")
        .selected
        .expect("a core set for this device");
    assert_eq!(
        fetched,
        ["manifestUrl", "coreReleaseUrl", "coreSetUrl"].map(|url| vector[url].as_str().unwrap())
    );
    assert_eq!(selected.core_set_id, set_vector["coreSetId"]);
    assert_eq!(selected.envelope, vector["coreSet"]);
    assert_eq!(
        selected.id,
        document_of(&vector, "coreRelease")["id"],
        "the release is named as its line names it"
    );
    let base = document_of(&vector, "coreRelease")["baseUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    for object in &selected.objects {
        assert_eq!(object.url, format!("{base}objects/{}", object.sha256));
    }

    // Current, another channel, another architecture: the manifest alone.
    for (arch, channel, highest) in [(arch, channel, 1), (arch, "lts", 0), ("arm64", channel, 0)] {
        let (verified, fetched) = run_core(&files(&vector), source, arch, channel, highest);
        assert!(verified.unwrap().selected.is_none());
        assert_eq!(fetched, [vector["manifestUrl"].as_str().unwrap()]);
    }
}

/// THE PROTOCOL VOCABULARY. The accepted strings are the ones the shared
/// fixtures carry; every other spelling is refused, with no aliases.
/// Neighbouring spellings are listed as refused by name, so a reader that
/// quietly took one fails here.
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
    assert_eq!(
        document_of(&vector, "manifest")["schema"],
        accepted["catalog"]
    );
    assert_eq!(
        document_of(&vector, "release")["schema"],
        accepted["release"]
    );
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
    let shared: Value =
        serde_json::from_str(include_str!("component-contracts/envelope.json")).unwrap();
    assert_eq!(shared["envelope"]["schema"], accepted["envelope"]);
    for spelling in refused["envelope"].as_array().unwrap() {
        let mut e: Value = shared["envelope"].clone();
        e["schema"] = spelling.clone();
        assert!(
            authenticate_deployment(&envelope(&e), &[trust()]).is_err(),
            "the envelope accepted {spelling}"
        );
    }

    // The system-only deployment is the same descriptor under its own name.
    let mut system_only = descriptor.clone();
    system_only["schema"] = accepted["deploymentSystemOnly"].clone();
    parse_deployment(serde_json::to_string(&system_only).unwrap().as_bytes())
        .expect("the system-only deployment schema parses");

    // The core set and the core release's document.
    let set_vector: Value =
        serde_json::from_str(include_str!("component-contracts/core-set.json")).unwrap();
    let payload: Value = serde_json::from_str(set_vector["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["schema"], accepted["coreSet"]);
    for spelling in refused["coreSet"].as_array().unwrap() {
        let mut p = payload.clone();
        p["schema"] = spelling.clone();
        assert!(
            core_set::parse(&serde_json::to_vec(&p).unwrap()).is_err(),
            "the core set accepted {spelling}"
        );
    }
    assert_eq!(
        document_of(&vector, "coreRelease")["schema"],
        accepted["coreRelease"]
    );
    for spelling in refused["coreRelease"].as_array().unwrap() {
        let mut document = document_of(&vector, "coreRelease");
        document["schema"] = spelling.clone();
        let mut files = files(&vector);
        files.insert(
            vector["coreReleaseUrl"].as_str().unwrap().to_owned(),
            serde_json::to_vec(&document).unwrap(),
        );
        assert!(
            run_core(
                &files,
                vector["source"].as_str().unwrap(),
                cases["valid"]["arch"].as_str().unwrap(),
                cases["coreSet"]["channel"].as_str().unwrap(),
                0
            )
            .0
            .is_err(),
            "the core release accepted {spelling}"
        );
    }

    // Refused: the manifest and release document strings, so the schema is
    // the only thing wrong.
    let source = vector["source"].as_str().unwrap();
    for (kind, url, name) in [
        ("catalog", "manifestUrl", "manifest"),
        ("release", "releaseUrl", "release"),
    ] {
        for spelling in refused[kind].as_array().unwrap() {
            let mut document = document_of(&vector, name);
            document["schema"] = spelling.clone();
            let mut files = files(&vector);
            files.insert(
                vector[url].as_str().unwrap().to_owned(),
                serde_json::to_vec(&document).unwrap(),
            );
            assert!(
                run(&files, &request(source, &cases)).0.is_err(),
                "the {kind} accepted {spelling}"
            );
        }
    }
}
