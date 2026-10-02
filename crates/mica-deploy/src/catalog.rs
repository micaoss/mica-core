//! Exact component selection from the server catalog.
//!
//! The catalog is unsigned: what a device trusts is each release's own signed
//! descriptor, the object digests that descriptor names, and the generation it
//! already runs. The catalog says which release is current and where each
//! file is; an address it names can make a transfer fail, never install other
//! bytes.
//!
//! Three documents, fetched in order and only as far as needed: the manifest
//! (`mica/catalog/v2`, one line per product) at the source; when the device's
//! line names a newer generation, that release's document (`mica/release/v1`);
//! then the signed descriptor it points at. Every link is a `path` relative
//! to the linking document's `baseUrl`.
//!
//! **Compatibility.** The configured source is an update root, and the reader
//! appends the manifest major it reads ([`MANIFEST_PATH`]), so a server keeps
//! serving every major a device in the field still reads, and a reader that
//! learns the next one finds it without a configuration change. Within a
//! major a document only grows: fields this reader does not know are ignored,
//! never refused. A change an old reader cannot ignore is a new major at a
//! new path, and the old major's current release is one whose reader takes
//! both.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::components::{Deployment, authenticate_deployment, component_id};

/// The manifest this reader takes, relative to the configured source.
pub const MANIFEST_PATH: &str = "v2/manifest.json";
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
/// The most product lines a manifest carries, and objects a release names.
const MAX_ENTRIES: usize = 128;
const MAX_ID_LEN: usize = 128;
const MAX_NOTES_CHARS: usize = 10000;
const MAX_URL_LEN: usize = 2048;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CatalogCheckpoint {
    pub source: String,
    pub revision: u64,
    pub payload_digest: String,
}

// The three wire documents. None refuses an unknown field: within a schema
// major a document only grows (module docs).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema: String,
    revision: u64,
    base_url: String,
    products: Vec<ProductLine>,
}
#[derive(Deserialize)]
struct ProductLine {
    product: String,
    board: String,
    latest: Latest,
}
#[derive(Deserialize)]
struct Latest {
    id: String,
    generation: u64,
    notes: String,
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseDocument {
    schema: String,
    base_url: String,
    id: String,
    product: String,
    board: String,
    generation: u64,
    notes: String,
    deployment: DescriptorLink,
    objects: Vec<ObjectLink>,
}
#[derive(Deserialize)]
struct DescriptorLink {
    path: String,
    sha256: String,
    bytes: u64,
}
/// An object as the release's document names it: its digest and length,
/// which the signed descriptor must name too, and where to fetch it.
#[derive(Deserialize)]
struct ObjectLink {
    sha256: String,
    bytes: u64,
    path: String,
}

#[derive(Debug, Serialize)]
pub struct SourceObject {
    pub sha256: String,
    pub bytes: u64,
    pub url: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedRelease {
    pub deployment_id: String,
    pub deployment: Deployment,
    pub envelope: String,
    pub objects: Vec<SourceObject>,
    pub notes: String,
}
pub struct VerifiedCatalog {
    pub checkpoint: CatalogCheckpoint,
    pub selected: Option<SelectedRelease>,
}
pub struct CatalogRequest<'a> {
    pub source: &'a str,
    pub board: &'a str,
    pub arch: &'a str,
    pub product: &'a str,
    pub checkpoint: Option<&'a CatalogCheckpoint>,
    pub highest_generation: u64,
}

pub fn artifacts(deployment: &Deployment) -> Result<BTreeMap<String, u64>> {
    let mut objects = BTreeMap::new();
    for artifact in deployment.artifacts() {
        if let Some(previous) = objects.insert(artifact.sha256.clone(), artifact.bytes) {
            ensure!(previous == artifact.bytes, "conflicting object lengths");
        }
    }
    Ok(objects)
}

/// The update root, as the device is configured with it: http or https, a
/// host, no user name, password, query or fragment, and a path ending in `/`.
/// The manifest is [`MANIFEST_PATH`] below it.
pub fn source_url(source: &str) -> Result<Url> {
    ensure!(source.len() <= MAX_URL_LEN, "source URL too long");
    let url = Url::parse(source)?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid catalog source URL"
    );
    ensure!(
        url.path().ends_with('/'),
        "the catalog source is an update root ending in `/`, such as https://res.micaos.dev/update/; this reader fetches {MANIFEST_PATH} below it"
    );
    Ok(url)
}

/// Where the manifest this reader takes is, below the configured source.
pub fn manifest_url(source: &str) -> Result<Url> {
    Ok(source_url(source)?.join(MANIFEST_PATH)?)
}

/// A document's `baseUrl`: a host, no user name, password, query or fragment,
/// a path ending in `/`, and https unless the source itself is http.
fn base_url(base: &str, source: &Url) -> Result<Url> {
    ensure!(base.len() <= MAX_URL_LEN, "baseUrl too long");
    let url = Url::parse(base).context("invalid baseUrl")?;
    ensure!(
        (url.scheme() == "https" || (url.scheme() == "http" && source.scheme() == "http"))
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path().ends_with('/')
            // Exactly as written: a URL the parser rewrites is not one the
            // server meant.
            && url.as_str() == base,
        "invalid baseUrl"
    );
    Ok(url)
}

/// `base` + `path`, where `path` stays below `base`: not empty, no leading
/// `/`, no `.` or `..` segment, no `?`, `#` or `\`.
fn link(base: &Url, path: &str) -> Result<Url> {
    ensure!(
        !path.is_empty()
            && path.len() <= MAX_URL_LEN
            && !path.starts_with('/')
            && !path.contains(['?', '#', '\\'])
            && path
                .split('/')
                .all(|segment| segment != "." && segment != ".."),
        "invalid path {path:?}"
    );
    let url = base.join(path)?;
    // A `scheme:` in the first segment would leave the base altogether.
    ensure!(
        url.as_str().starts_with(base.as_str()),
        "path {path:?} leaves its baseUrl"
    );
    Ok(url)
}

fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse a document in its exact canonical wire form.
fn canonical<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    ensure!(bytes.len() <= MAX_CATALOG_BYTES, "{what} too large");
    let raw: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(serde_json::to_vec(&raw)? == bytes, "noncanonical {what}");
    Ok(serde_json::from_value(raw)?)
}

/// Verify the catalog for one device, fetching each document through `fetch`
/// (its URL and the most bytes it may have): the manifest, and only when the
/// device's line names a newer generation, the release's document and its
/// signed descriptor.
pub fn verify_catalog(
    keys: &[[u8; 32]],
    request: &CatalogRequest<'_>,
    mut fetch: impl FnMut(&Url, u64) -> Result<Vec<u8>>,
) -> Result<VerifiedCatalog> {
    let source = source_url(request.source)?;
    let bytes = fetch(&manifest_url(request.source)?, MAX_CATALOG_BYTES as u64)?;
    let manifest: Manifest = canonical(&bytes, "catalog")?;
    ensure!(
        manifest.schema == "mica/catalog/v2"
            && manifest.revision > 0
            && manifest.revision <= 9_007_199_254_740_991
            && manifest.products.len() <= MAX_ENTRIES,
        "invalid catalog schema or bounds"
    );
    // A consistency check against a confused mirror, not a security control:
    // the document is unsigned, so anything that can rewrite it can also
    // rewrite its revision. Rollback protection is the generation comparison
    // below, against what this device already runs.
    let checkpoint = CatalogCheckpoint {
        source: request.source.to_owned(),
        revision: manifest.revision,
        payload_digest: hex::encode(aws_lc_rs::digest::digest(
            &aws_lc_rs::digest::SHA256,
            &bytes,
        )),
    };
    if let Some(previous) = request
        .checkpoint
        .filter(|previous| previous.source == request.source)
    {
        ensure!(
            checkpoint.revision >= previous.revision,
            "catalog revision rollback"
        );
        ensure!(
            checkpoint.revision != previous.revision
                || checkpoint.payload_digest == previous.payload_digest,
            "catalog revision changed its contents"
        );
    }
    let manifest_base = base_url(&manifest.base_url, &source)?;
    let mut lines = BTreeSet::new();
    let mut own = None;
    for line in manifest.products {
        ensure!(
            [&line.product, &line.board, &line.latest.id]
                .iter()
                .all(|value| !value.is_empty() && value.len() <= MAX_ID_LEN)
                && line.latest.notes.chars().count() <= MAX_NOTES_CHARS,
            "invalid product line"
        );
        let url = link(&manifest_base, &line.latest.path)?;
        ensure!(
            lines.insert((line.board.clone(), line.product.clone())),
            "two lines for one board and product"
        );
        if line.board == request.board && line.product == request.product {
            own = Some((line, url));
        }
    }
    // Current, or not offered: the manifest is all a device downloads.
    let Some((line, release_url)) =
        own.filter(|(line, _)| line.latest.generation > request.highest_generation)
    else {
        return Ok(VerifiedCatalog {
            checkpoint,
            selected: None,
        });
    };

    let release: ReleaseDocument = canonical(
        &fetch(&release_url, MAX_CATALOG_BYTES as u64)?,
        "release document",
    )?;
    ensure!(
        release.schema == "mica/release/v1",
        "invalid release document schema"
    );
    ensure!(
        release.id == line.latest.id
            && release.board == line.board
            && release.product == line.product
            && release.generation == line.latest.generation
            && release.notes.chars().count() <= MAX_NOTES_CHARS,
        "the release document does not match its manifest line"
    );
    ensure!(
        release.objects.len() <= MAX_ENTRIES,
        "invalid release document bounds"
    );
    let release_base = base_url(&release.base_url, &source)?;
    let link_to = &release.deployment;
    ensure!(
        sha256_hex(&link_to.sha256)
            && link_to.bytes > 0
            && link_to.bytes <= MAX_CATALOG_BYTES as u64,
        "invalid descriptor link"
    );
    let descriptor_url = link(&release_base, &link_to.path)?;

    let envelope = fetch(&descriptor_url, link_to.bytes)?;
    ensure!(
        envelope.len() as u64 == link_to.bytes
            && hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                &envelope
            )) == link_to.sha256,
        "the descriptor does not match its digest and length"
    );
    let deployment = authenticate_deployment(&envelope, keys)?;
    ensure!(
        deployment.board == line.board
            && deployment.product == line.product
            && deployment.generation == line.latest.generation
            && deployment.arch == request.arch,
        "the signed descriptor does not match its manifest line"
    );
    let mut required = artifacts(&deployment)?;
    ensure!(
        release.objects.len() == required.len(),
        "missing or extra component objects"
    );
    let mut objects = Vec::with_capacity(release.objects.len());
    for object in release.objects {
        ensure!(
            required.remove(&object.sha256) == Some(object.bytes),
            "component object substitution"
        );
        objects.push(SourceObject {
            url: link(&release_base, &object.path)?.into(),
            sha256: object.sha256,
            bytes: object.bytes,
        });
    }
    Ok(VerifiedCatalog {
        checkpoint,
        selected: Some(SelectedRelease {
            deployment_id: component_id(&serde_json::to_value(&deployment)?)?,
            deployment,
            envelope: String::from_utf8(envelope).context("descriptor is not UTF-8")?,
            objects,
            notes: release.notes,
        }),
    })
}
