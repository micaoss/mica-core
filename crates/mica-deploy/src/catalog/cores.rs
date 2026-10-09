//! The core line of the catalog.
//!
//! The manifest's `cores` names, per channel and architecture, the current
//! core release; that release's document (`mica/core-release/v1`) names the
//! signed `mica/core-set/v1` and the set's objects. Read exactly as a product
//! line is: the manifest alone when the device is current or its channel has
//! no line, and every later document checked against the one that linked it.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{
    CatalogCheckpoint, DescriptorLink, MAX_CATALOG_BYTES, MAX_ENTRIES, MAX_ID_LEN, ObjectLink,
    SourceObject, base_url, canonical, link, read_manifest, sha256_hex,
};
use crate::core_set::{self, CoreSet};

/// One channel's current core release for one architecture.
#[derive(Deserialize)]
pub(super) struct CoreLine {
    channel: String,
    arch: String,
    latest: CoreLatest,
}
#[derive(Deserialize)]
struct CoreLatest {
    /// The release's name, the same on every architecture's line.
    id: String,
    generation: u64,
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoreReleaseDocument {
    schema: String,
    base_url: String,
    id: String,
    channel: String,
    arch: String,
    generation: u64,
    core_set: DescriptorLink,
    objects: Vec<ObjectLink>,
}

/// The core set a device takes, with where each of its objects is.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedCoreSet {
    /// The release's name in the catalog, for people and logs.
    pub id: String,
    /// The set's own identity: the digest of its signed payload.
    pub core_set_id: String,
    pub set: CoreSet,
    pub envelope: String,
    pub objects: Vec<SourceObject>,
}
pub struct VerifiedCoreCatalog {
    pub checkpoint: CatalogCheckpoint,
    pub selected: Option<SelectedCoreSet>,
}
pub struct CoreCatalogRequest<'a> {
    pub source: &'a str,
    pub arch: &'a str,
    pub channel: &'a str,
    pub checkpoint: Option<&'a CatalogCheckpoint>,
    /// The generation of the set the device holds on this channel; zero when
    /// it holds none of it, as after a channel switch.
    pub highest_generation: u64,
}

/// Verify the catalog's core line for one device, fetching each document
/// through `fetch`: the manifest, and only when the line of the device's
/// channel and architecture names a newer generation, the core release's
/// document and the signed set.
pub fn verify_core_catalog(
    keys: &[[u8; 32]],
    request: &CoreCatalogRequest<'_>,
    mut fetch: impl FnMut(&Url, u64) -> Result<Vec<u8>>,
) -> Result<VerifiedCoreCatalog> {
    let (manifest, checkpoint, source, manifest_base) =
        read_manifest(request.source, request.checkpoint, &mut fetch)?;
    let mut lines = BTreeSet::new();
    let mut own = None;
    for line in manifest.cores {
        ensure!(
            [&line.channel, &line.arch, &line.latest.id]
                .iter()
                .all(|value| !value.is_empty() && value.len() <= MAX_ID_LEN),
            "invalid core line"
        );
        let url = link(&manifest_base, &line.latest.path)?;
        ensure!(
            lines.insert((line.channel.clone(), line.arch.clone())),
            "two core lines for one channel and architecture"
        );
        if line.channel == request.channel && line.arch == request.arch {
            own = Some((line, url));
        }
    }
    let Some((line, release_url)) =
        own.filter(|(line, _)| line.latest.generation > request.highest_generation)
    else {
        return Ok(VerifiedCoreCatalog {
            checkpoint,
            selected: None,
        });
    };

    let release: CoreReleaseDocument = canonical(
        &fetch(&release_url, MAX_CATALOG_BYTES as u64)?,
        "core release document",
    )?;
    ensure!(
        release.schema == "mica/core-release/v1",
        "invalid core release document schema"
    );
    ensure!(
        release.id == line.latest.id
            && release.channel == line.channel
            && release.arch == line.arch
            && release.generation == line.latest.generation,
        "the core release document does not match its manifest line"
    );
    ensure!(
        release.objects.len() <= MAX_ENTRIES,
        "invalid core release document bounds"
    );
    let release_base = base_url(&release.base_url, &source)?;
    let link_to = &release.core_set;
    ensure!(
        sha256_hex(&link_to.sha256)
            && link_to.bytes > 0
            && link_to.bytes <= MAX_CATALOG_BYTES as u64,
        "invalid core set link"
    );
    let envelope = fetch(&link(&release_base, &link_to.path)?, link_to.bytes)?;
    ensure!(
        envelope.len() as u64 == link_to.bytes
            && hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                &envelope
            )) == link_to.sha256,
        "the core set does not match its digest and length"
    );
    let set = core_set::authenticate(&envelope, keys)?;
    ensure!(
        set.channel == line.channel
            && set.arch == line.arch
            && set.generation == line.latest.generation,
        "the signed core set does not match its manifest line"
    );
    let mut required = BTreeMap::new();
    for artifact in set.artifacts() {
        if let Some(previous) = required.insert(artifact.sha256.clone(), artifact.bytes) {
            ensure!(previous == artifact.bytes, "conflicting object lengths");
        }
    }
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
    Ok(VerifiedCoreCatalog {
        checkpoint,
        selected: Some(SelectedCoreSet {
            id: release.id,
            core_set_id: set.id()?,
            set,
            envelope: String::from_utf8(envelope).context("core set is not UTF-8")?,
            objects,
        }),
    })
}
