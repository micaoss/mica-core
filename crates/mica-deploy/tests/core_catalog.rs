//! The core line of the catalog: `cores` in `mica/catalog/v2`, the `mica/core-release/v1` document it
//! links, and the signed `mica/core-set/v1` the device takes from it.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::catalog::{CoreCatalogRequest, VerifiedCoreCatalog, verify_core_catalog};
use serde_json::{Value, json};

const SOURCE: &str = "https://updates.test/update/";
const MANIFEST: &str = "https://updates.test/update/v2/manifest.json";
const DOWNLOADS: &str = "https://dl.test/";
/// A core release has one directory per architecture, each with its own
/// document, signed set and objects.
const RELEASE_BASE: &str = "https://dl.test/mica/core.general/20261010-0000/amd64/";
const RELEASE: &str = "https://dl.test/mica/core.general/20261010-0000/amd64/index.json";
const SET_URL: &str = "https://dl.test/mica/core.general/20261010-0000/amd64/core-set.json";

fn signer() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap()
}
fn public() -> [u8; 32] {
    signer().public_key().as_ref().try_into().unwrap()
}
fn signed(value: &Value) -> Vec<u8> {
    let key = signer();
    let payload = serde_json::to_vec(value).unwrap();
    format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        sha256(key.public_key().as_ref()),
        STANDARD.encode(&payload),
        STANDARD.encode(key.sign(&payload).as_ref())
    )
    .into_bytes()
}
fn sha256(bytes: &[u8]) -> String {
    hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes))
}
fn core_set() -> Value {
    let vector: Value =
        serde_json::from_str(include_str!("component-contracts/core-set.json")).unwrap();
    serde_json::from_str(vector["payload"].as_str().unwrap()).unwrap()
}

/// What the origin serves: the manifest, the core release's document and the signed set.
struct Origin {
    manifest: Value,
    release: Value,
    envelope: Vec<u8>,
}

impl Origin {
    fn new() -> Self {
        let set = core_set();
        let envelope = signed(&set);
        let mut objects = BTreeMap::new();
        for component in set["components"].as_array().unwrap() {
            for part in ["image", "signature"] {
                let artifact = &component["content"][part];
                let sha = artifact["sha256"].as_str().unwrap();
                objects.insert(
                    sha.to_owned(),
                    json!({"sha256": sha, "bytes": artifact["bytes"], "path": format!("objects/{sha}")}),
                );
            }
        }
        Self {
            manifest: json!({"schema": "mica/catalog/v2", "revision": 3, "baseUrl": DOWNLOADS,
            "products": [],
            "cores": [
                {"channel": "general", "arch": "arm64",
                    "latest": {"id": "core.general.20261010-0000", "generation": 1,
                        "path": "mica/core.general/20261010-0000/arm64/index.json"}},
                {"channel": "general", "arch": "amd64",
                    "latest": {"id": "core.general.20261010-0000", "generation": 1,
                        "path": "mica/core.general/20261010-0000/amd64/index.json"}},
            ]}),
            release: json!({"schema": "mica/core-release/v1", "baseUrl": RELEASE_BASE,
                "id": "core.general.20261010-0000", "channel": "general", "arch": "amd64", "generation": 1,
                "coreSet": {"path": "core-set.json",
                    "sha256": sha256(&envelope), "bytes": envelope.len()},
                "objects": objects.into_values().collect::<Vec<_>>()}),
            envelope,
        }
    }

    fn verify(
        &self,
        channel: &str,
        highest: u64,
    ) -> (anyhow::Result<VerifiedCoreCatalog>, Vec<String>) {
        let fetched = RefCell::new(Vec::new());
        let result = verify_core_catalog(
            &[public()],
            &CoreCatalogRequest {
                source: SOURCE,
                arch: "amd64",
                channel,
                checkpoint: None,
                highest_generation: highest,
            },
            |url, _limit| {
                fetched.borrow_mut().push(url.to_string());
                Ok(match url.as_str() {
                    MANIFEST => serde_json::to_vec(&self.manifest).unwrap(),
                    RELEASE => serde_json::to_vec(&self.release).unwrap(),
                    SET_URL => self.envelope.clone(),
                    other => anyhow::bail!("unexpected fetch {other}"),
                })
            },
        );
        (result, fetched.into_inner())
    }
}

fn refusal(origin: &Origin) -> String {
    match origin.verify("general", 0).0 {
        Ok(_) => panic!("accepted"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn a_device_takes_its_channel_and_architecture_set_with_every_object() {
    let (result, fetched) = Origin::new().verify("general", 0);
    let selected = result.unwrap().selected.unwrap();
    assert_eq!(fetched, [MANIFEST, RELEASE, SET_URL]);
    assert_eq!(selected.id, "core.general.20261010-0000");
    assert_eq!(selected.set.generation, 1);
    // The golden components share one content, so the set names two objects.
    assert_eq!(selected.objects.len(), 2);
    assert!(
        selected
            .objects
            .iter()
            .all(|o| o.url == format!("{RELEASE_BASE}objects/{}", o.sha256))
    );
}

#[test]
fn a_current_device_or_one_on_a_channel_with_no_line_reads_only_the_manifest() {
    let origin = Origin::new();
    let (current, fetched) = origin.verify("general", 1);
    assert!(current.unwrap().selected.is_none());
    assert_eq!(fetched, [MANIFEST]);
    let (other, fetched) = origin.verify("lts", 0);
    assert!(other.unwrap().selected.is_none());
    assert_eq!(fetched, [MANIFEST]);
}

#[test]
fn a_manifest_without_cores_still_reads() {
    let mut origin = Origin::new();
    origin.manifest.as_object_mut().unwrap().remove("cores");
    assert!(origin.verify("general", 0).0.unwrap().selected.is_none());
}

#[test]
fn a_document_that_disagrees_with_its_line_or_its_set_is_refused() {
    let mut wrong_arch = Origin::new();
    wrong_arch.release["arch"] = json!("arm64");
    assert!(
        refusal(&wrong_arch).contains("the core release document does not match its manifest line")
    );

    let mut wrong_digest = Origin::new();
    wrong_digest.release["coreSet"]["sha256"] = json!("0".repeat(64));
    assert!(refusal(&wrong_digest).contains("the core set does not match its digest and length"));

    let mut missing = Origin::new();
    missing.release["objects"].as_array_mut().unwrap().pop();
    assert!(refusal(&missing).contains("missing or extra component objects"));

    let mut generation = Origin::new();
    generation.manifest["cores"][1]["latest"]["generation"] = json!(2);
    generation.release["generation"] = json!(2);
    assert!(refusal(&generation).contains("the signed core set does not match its manifest line"));

    let mut twice = Origin::new();
    let line = twice.manifest["cores"][1].clone();
    twice.manifest["cores"].as_array_mut().unwrap().push(line);
    assert!(refusal(&twice).contains("two core lines for one channel and architecture"));
}
