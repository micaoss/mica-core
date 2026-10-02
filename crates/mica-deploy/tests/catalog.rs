// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::catalog::{CatalogCheckpoint, CatalogRequest, VerifiedCatalog, verify_catalog};
use serde_json::{Value, json};

const SOURCE: &str = "https://updates.test/update/";
const MANIFEST: &str = "https://updates.test/update/v2/manifest.json";
/// Files live on another host than the manifest.
const DOWNLOADS: &str = "https://dl.test/";
const RELEASE_BASE: &str = "https://dl.test/mica/uefi-x64-dev/1/";
const RELEASE: &str = "https://dl.test/mica/uefi-x64-dev/1/index.json";
const DESCRIPTOR: &str = "https://dl.test/mica/uefi-x64-dev/1/deployment.json";

fn signer() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap()
}
fn signed(value: &Value) -> Vec<u8> {
    let key = signer();
    let payload = serde_json::to_vec(value).unwrap();
    format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, key.public_key().as_ref())),
        STANDARD.encode(&payload), STANDARD.encode(key.sign(&payload).as_ref())
    ).into_bytes()
}
fn sha256(bytes: &[u8]) -> String {
    hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes))
}
fn descriptor() -> Value {
    serde_json::from_str(include_str!("component-contracts/deployment.json")).unwrap()
}

/// What the origin serves: the manifest, the release's document and the
/// signed descriptor, as values a test may change before they are served.
struct Origin {
    manifest: Value,
    release: Value,
    envelope: Vec<u8>,
}

impl Origin {
    fn new() -> Self {
        Self::signing(&descriptor())
    }

    fn signing(deployment: &Value) -> Self {
        let envelope = signed(deployment);
        let mut objects = BTreeMap::new();
        for pointer in [
            "/kernel/boot/artifact",
            "/kernel/support/image",
            "/kernel/support/signature",
            "/rootfs/content/image",
            "/rootfs/content/signature",
        ] {
            let artifact = deployment.pointer(pointer).unwrap();
            let sha = artifact["sha256"].as_str().unwrap();
            objects.insert(
                sha.to_owned(),
                json!({"sha256": sha, "bytes": artifact["bytes"], "path": format!("objects/{sha}")}),
            );
        }
        Self {
            manifest: json!({"schema": "mica/catalog/v2", "revision": 2, "baseUrl": DOWNLOADS,
                "products": [{"product": "uefi-x64-dev", "board": "uefi-x64", "variant": "dev",
                    "latest": {"id": "release-1", "generation": 1, "notes": "Test",
                        "path": "mica/uefi-x64-dev/1/index.json"},
                    "releases": "catalog/uefi-x64-dev/releases.json"}]}),
            release: json!({"schema": "mica/release/v1", "baseUrl": RELEASE_BASE,
                "id": "release-1", "product": "uefi-x64-dev", "board": "uefi-x64", "variant": "dev",
                "version": "dev-1", "generation": 1, "notes": "Test",
                "publishedAt": "2026-10-02T00:00:00.000Z", "files": [],
                "deployment": {"path": "deployment.json", "sha256": sha256(&envelope), "bytes": envelope.len()},
                "objects": objects.into_values().collect::<Vec<_>>()}),
            envelope,
        }
    }

    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (MANIFEST.into(), serde_json::to_vec(&self.manifest).unwrap()),
            (RELEASE.into(), serde_json::to_vec(&self.release).unwrap()),
            (DESCRIPTOR.into(), self.envelope.clone()),
        ])
    }
}

fn trusted() -> [u8; 32] {
    signer().public_key().as_ref().try_into().unwrap()
}
fn request<'a>(
    product: &'a str,
    checkpoint: Option<&'a CatalogCheckpoint>,
    highest: u64,
) -> CatalogRequest<'a> {
    CatalogRequest {
        source: SOURCE,
        board: "uefi-x64",
        arch: "amd64",
        product,
        checkpoint,
        highest_generation: highest,
    }
}

/// Verify against `files`, returning the result and every URL fetched.
fn run(
    files: &BTreeMap<String, Vec<u8>>,
    keys: &[[u8; 32]],
    request: &CatalogRequest<'_>,
) -> (anyhow::Result<VerifiedCatalog>, Vec<String>) {
    let fetched = RefCell::new(Vec::new());
    let result = verify_catalog(keys, request, |url, limit| {
        fetched.borrow_mut().push(url.to_string());
        let bytes = files
            .get(url.as_str())
            .ok_or_else(|| anyhow::anyhow!("404 {url}"))?;
        anyhow::ensure!(bytes.len() as u64 <= limit, "over the limit");
        Ok(bytes.clone())
    });
    (result, fetched.into_inner())
}
fn verify(
    origin: &Origin,
    checkpoint: Option<&CatalogCheckpoint>,
    highest: u64,
) -> anyhow::Result<VerifiedCatalog> {
    run(
        &origin.files(),
        &[trusted()],
        &request("uefi-x64-dev", checkpoint, highest),
    )
    .0
}

#[test]
fn a_newer_release_is_selected_through_its_document_and_descriptor() {
    let origin = Origin::new();
    let (verified, fetched) = run(
        &origin.files(),
        &[trusted()],
        &request("uefi-x64-dev", None, 0),
    );
    let selected = verified.unwrap().selected.unwrap();
    assert_eq!(fetched, [MANIFEST, RELEASE, DESCRIPTOR]);
    assert_eq!(selected.deployment.generation, 1);
    assert_eq!(selected.envelope.as_bytes(), origin.envelope);
    assert_eq!(selected.notes, "Test");
    assert!(!selected.objects.is_empty());
    for object in &selected.objects {
        assert_eq!(
            object.url,
            format!("{RELEASE_BASE}objects/{}", object.sha256)
        );
    }
}

/// A device that is current, or whose product is not offered, reads the
/// manifest and nothing else.
#[test]
fn a_current_device_reads_only_the_manifest() {
    let files = Origin::new().files();
    for request in [
        request("uefi-x64-dev", None, 1),
        request("uefi-x64-prod", None, 0),
        CatalogRequest {
            board: "uefi-arm64",
            ..request("uefi-x64-dev", None, 0)
        },
    ] {
        let (verified, fetched) = run(&files, &[trusted()], &request);
        assert!(verified.unwrap().selected.is_none());
        assert_eq!(fetched, [MANIFEST]);
    }
}

/// Within a major a document only grows: a field this reader does not know,
/// at any level, is ignored.
#[test]
fn unknown_fields_are_ignored_in_every_unsigned_document() {
    let mut origin = Origin::new();
    origin.manifest["expiresAt"] = json!("2026-09-10T00:00:00.000Z");
    origin.manifest["products"][0]["channel"] = json!("stable");
    origin.manifest["products"][0]["latest"]["size"] = json!(1);
    origin.release["mirrors"] = json!([]);
    origin.release["deployment"]["mediaType"] = json!("application/json");
    origin.release["objects"][0]["compression"] = json!("none");
    assert!(verify(&origin, None, 0).unwrap().selected.is_some());
}

#[test]
fn refuses_rollback_equivocation_other_majors_noncanonical_and_untrusted_documents() {
    let origin = Origin::new();
    let verified = verify(&origin, None, 0).unwrap();
    let mut older = Origin::new();
    older.manifest["revision"] = json!(1);
    assert!(verify(&older, Some(&verified.checkpoint), 0).is_err());
    let mut changed = Origin::new();
    changed.manifest["products"][0]["latest"]["notes"] = json!("changed without a new revision");
    assert!(verify(&changed, Some(&verified.checkpoint), 0).is_err());
    for schema in [
        "mica/catalog/v1",
        "mica/catalog/v3",
        "mica/update-catalog/v1",
    ] {
        let mut other = Origin::new();
        other.manifest["schema"] = json!(schema);
        assert!(verify(&other, None, 0).is_err(), "{schema}");
    }
    for schema in ["mica/release/v2", "mica/catalog/v2"] {
        let mut other = Origin::new();
        other.release["schema"] = json!(schema);
        assert!(verify(&other, None, 0).is_err(), "{schema}");
    }
    let mut files = origin.files();
    files.insert(
        MANIFEST.into(),
        serde_json::to_vec_pretty(&origin.manifest).unwrap(),
    );
    assert!(
        run(&files, &[trusted()], &request("uefi-x64-dev", None, 0))
            .0
            .is_err()
    );
    let mut files = origin.files();
    files.insert(
        RELEASE.into(),
        serde_json::to_vec_pretty(&origin.release).unwrap(),
    );
    assert!(
        run(&files, &[trusted()], &request("uefi-x64-dev", None, 0))
            .0
            .is_err()
    );
    // The descriptor is still authenticated: an untrusted key refuses it.
    assert!(
        run(
            &origin.files(),
            &[[0; 32]],
            &request("uefi-x64-dev", None, 0)
        )
        .0
        .is_err()
    );
}

/// The release's document must be the one its manifest line names, and the
/// descriptor the one the document names, byte for byte.
#[test]
fn refuses_a_document_or_descriptor_that_does_not_match_its_link() {
    for (pointer, replacement) in [
        ("/id", json!("release-2")),
        ("/board", json!("uefi-arm64")),
        ("/product", json!("uefi-x64-prod")),
        ("/generation", json!(2)),
        ("/deployment/sha256", json!("00".repeat(32))),
        ("/deployment/sha256", json!("AB".repeat(32))),
        ("/deployment/bytes", json!(1)),
        ("/deployment/bytes", json!(2 * 1024 * 1024)),
        ("/objects/0/bytes", json!(1)),
        ("/objects/0/sha256", json!("00".repeat(32))),
    ] {
        let mut origin = Origin::new();
        *origin.release.pointer_mut(pointer).unwrap() = replacement;
        assert!(verify(&origin, None, 0).is_err(), "{pointer}");
    }
    let mut origin = Origin::new();
    origin.release["objects"].as_array_mut().unwrap().pop();
    assert!(verify(&origin, None, 0).is_err(), "a missing object");
    let mut origin = Origin::new();
    let duplicate = origin.release["objects"][0].clone();
    origin.release["objects"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(verify(&origin, None, 0).is_err(), "a duplicate object");
    // A descriptor that is signed but names another generation than the line.
    let mut deployment = descriptor();
    deployment["generation"] = json!(2);
    let mut origin = Origin::signing(&deployment);
    origin.manifest["products"][0]["latest"]["generation"] = json!(1);
    origin.release["generation"] = json!(1);
    assert!(
        verify(&origin, None, 0).is_err(),
        "a signed generation the line does not name"
    );
}

#[test]
fn refuses_two_lines_for_one_product() {
    let mut origin = Origin::new();
    let line = origin.manifest["products"][0].clone();
    origin.manifest["products"]
        .as_array_mut()
        .unwrap()
        .push(line);
    assert!(verify(&origin, None, 0).is_err());
}

/// A link is `baseUrl` + `path`, and both are held to the rule even when the
/// device would never follow them: a manifest is refused as a whole.
#[test]
fn refuses_base_urls_and_paths_that_leave_their_rule() {
    for base in [
        "http://dl.test/",
        "ftp://dl.test/",
        "https://user@dl.test/",
        "https://dl.test/?sig=abc",
        "https://dl.test/#part",
        "https://dl.test/mica",
        "https://DL.test/",
        "dl.test/",
    ] {
        let mut origin = Origin::new();
        origin.manifest["baseUrl"] = json!(base);
        assert!(verify(&origin, None, 1).is_err(), "manifest baseUrl {base}");
        let mut origin = Origin::new();
        origin.release["baseUrl"] = json!(base);
        assert!(verify(&origin, None, 0).is_err(), "release baseUrl {base}");
    }
    for path in [
        "",
        "/mica/uefi-x64-dev/1/index.json",
        "mica/../index.json",
        "mica/./index.json",
        "mica/uefi-x64-dev/1/index.json?x=1",
        "mica/uefi-x64-dev/1/index.json#x",
        "mica\\uefi-x64-dev",
        "https://elsewhere.test/index.json",
        "javascript:alert(1)",
    ] {
        let mut origin = Origin::new();
        origin.manifest["products"][0]["latest"]["path"] = json!(path);
        assert!(verify(&origin, None, 1).is_err(), "line path {path:?}");
        let mut origin = Origin::new();
        origin.release["deployment"]["path"] = json!(path);
        assert!(
            verify(&origin, None, 0).is_err(),
            "descriptor path {path:?}"
        );
        let mut origin = Origin::new();
        origin.release["objects"][0]["path"] = json!(path);
        assert!(verify(&origin, None, 0).is_err(), "object path {path:?}");
    }
}

/// The source is an update root; the reader appends the major it reads.
#[test]
fn the_source_is_an_update_root() {
    let files = Origin::new().files();
    for source in [
        "https://updates.test/update/manifest.json",
        "https://updates.test/update",
        "https://updates.test/update/?x=1",
        "https://user@updates.test/update/",
        "ftp://updates.test/update/",
    ] {
        let (verified, fetched) = run(
            &files,
            &[trusted()],
            &CatalogRequest {
                source,
                ..request("uefi-x64-dev", None, 0)
            },
        );
        assert!(verified.is_err(), "{source}");
        assert!(fetched.is_empty(), "{source}");
    }
    assert_eq!(
        mica_deploy::catalog::manifest_url(SOURCE).unwrap().as_str(),
        MANIFEST
    );
}
