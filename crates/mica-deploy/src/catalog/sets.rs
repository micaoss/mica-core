//! The set lines of the catalog.
//!
//! The manifest's `sets` names, per kind, channel and architecture, the
//! current set release; that release's document (`mica/set-release/v1`) names
//! the signed `mica/set/v1` and the set's objects. Read exactly as a product
//! line is: the manifest alone when the device is current or its channel has
//! no line, and every later document checked against the one that linked it.
//! A line of a kind this reader does not implement is not its to judge, and
//! is passed over.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{
    CatalogCheckpoint, DescriptorLink, MAX_CATALOG_BYTES, MAX_ENTRIES, MAX_ID_LEN, ObjectLink,
    SourceObject, base_url, canonical, link, read_manifest, sha256_hex,
};
use crate::update_set::{self, SetKind, UpdateSet};

/// One channel's current release of one kind for one architecture.
#[derive(Deserialize)]
pub(super) struct SetLine {
    /// A name, not a [`SetKind`]: the manifest may name kinds newer than this reader.
    kind: String,
    channel: String,
    arch: String,
    latest: SetLatest,
}
#[derive(Deserialize)]
struct SetLatest {
    /// The release's name, the same on every architecture's line.
    id: String,
    generation: u64,
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetReleaseDocument {
    schema: String,
    base_url: String,
    id: String,
    kind: String,
    channel: String,
    arch: String,
    generation: u64,
    set: DescriptorLink,
    objects: Vec<ObjectLink>,
}

/// The set a device takes, with where each of its objects is.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedSet {
    /// The release's name in the catalog, for people and logs.
    pub id: String,
    /// The set's own identity: the digest of its signed payload.
    pub set_id: String,
    pub set: UpdateSet,
    pub envelope: String,
    pub objects: Vec<SourceObject>,
}
pub struct VerifiedSetCatalog {
    pub checkpoint: CatalogCheckpoint,
    pub selected: Option<SelectedSet>,
    /// A newer set that does not select on the root it was checked for.
    pub waits_for_system: Option<WaitingSet>,
}

/// A newer set not offered, and why: what of the root it was checked for
/// one of its components does not accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingSet {
    pub id: String,
    pub generation: u64,
    pub reason: String,
}
pub struct SetCatalogRequest<'a> {
    pub source: &'a str,
    pub kind: SetKind,
    pub arch: &'a str,
    pub channel: &'a str,
    pub checkpoint: Option<&'a CatalogCheckpoint>,
    /// The generation of the set the device holds on this channel; zero when
    /// it holds none of it, as after a channel switch.
    pub highest_generation: u64,
}

/// Verify the catalog's line of one kind for one device, fetching each
/// document through `fetch`: the manifest, and only when the line of the
/// device's channel and architecture names a newer generation, the set
/// release's document and the signed set.
pub fn verify_set_catalog(
    keys: &[[u8; 32]],
    request: &SetCatalogRequest<'_>,
    mut fetch: impl FnMut(&Url, u64) -> Result<Vec<u8>>,
) -> Result<VerifiedSetCatalog> {
    let (manifest, checkpoint, source, manifest_base) =
        read_manifest(request.source, request.checkpoint, &mut fetch)?;
    let mut lines = BTreeSet::new();
    let mut own = None;
    for line in manifest.sets {
        ensure!(
            [&line.kind, &line.channel, &line.arch, &line.latest.id]
                .iter()
                .all(|value| !value.is_empty() && value.len() <= MAX_ID_LEN),
            "invalid set line"
        );
        let url = link(&manifest_base, &line.latest.path)?;
        ensure!(
            lines.insert((line.kind.clone(), line.channel.clone(), line.arch.clone())),
            "two set lines for one kind, channel and architecture"
        );
        if line.kind == request.kind.as_str()
            && line.channel == request.channel
            && line.arch == request.arch
        {
            own = Some((line, url));
        }
    }
    let Some((line, release_url)) =
        own.filter(|(line, _)| line.latest.generation > request.highest_generation)
    else {
        return Ok(VerifiedSetCatalog {
            checkpoint,
            selected: None,
            waits_for_system: None,
        });
    };

    let release: SetReleaseDocument = canonical(
        &fetch(&release_url, MAX_CATALOG_BYTES as u64)?,
        "set release document",
    )?;
    ensure!(
        release.schema == "mica/set-release/v1",
        "invalid set release document schema"
    );
    ensure!(
        release.id == line.latest.id
            && release.kind == line.kind
            && release.channel == line.channel
            && release.arch == line.arch
            && release.generation == line.latest.generation,
        "the set release document does not match its manifest line"
    );
    ensure!(
        release.objects.len() <= MAX_ENTRIES,
        "invalid set release document bounds"
    );
    let release_base = base_url(&release.base_url, &source)?;
    let link_to = &release.set;
    ensure!(
        sha256_hex(&link_to.sha256)
            && link_to.bytes > 0
            && link_to.bytes <= MAX_CATALOG_BYTES as u64,
        "invalid set link"
    );
    let envelope = fetch(&link(&release_base, &link_to.path)?, link_to.bytes)?;
    ensure!(
        envelope.len() as u64 == link_to.bytes
            && hex::encode(aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                &envelope
            )) == link_to.sha256,
        "the set does not match its digest and length"
    );
    let set = update_set::authenticate(&envelope, keys)?;
    ensure!(
        set.kind == request.kind
            && set.channel == line.channel
            && set.arch == line.arch
            && set.generation == line.latest.generation,
        "the signed set does not match its manifest line"
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
    Ok(VerifiedSetCatalog {
        checkpoint,
        selected: Some(SelectedSet {
            id: release.id,
            set_id: set.id()?,
            set,
            envelope: String::from_utf8(envelope).context("set is not UTF-8")?,
            objects,
        }),
        waits_for_system: None,
    })
}
