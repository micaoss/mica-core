//! Acquiring a set: the catalog's line of its kind, and a set archive.
//!
//! The same workspace, budget, object store and delta transfer as a
//! deployment's. What is left ready is the signed set and its objects, for
//! `mica-deploy set-install`.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use super::*;
use crate::catalog::{SelectedSet, SetCatalogRequest, VerifiedSetCatalog, WaitingSet};
use crate::core_set;
use crate::deployments::SetTarget;
use crate::set_state;
use crate::update_set::{self, SetKind, UpdateSet};

/// A set whose signed envelope and every object are in the workspace.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadySet {
    pub id: String,
    /// The kind the set was signed as.
    pub set_kind: SetKind,
    pub path: PathBuf,
    pub objects: PathBuf,
    pub channel: String,
    pub version: String,
    pub generation: u64,
}

/// What an archive turned out to carry.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Imported {
    Deployment(ReadyDeployment),
    Set(ReadySet),
}

/// The schema an envelope's payload names, read without authenticating it:
/// enough to say which reader the bytes go to, and nothing more.
fn payload_schema(envelope: &[u8]) -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let outer: serde_json::Value = serde_json::from_slice(envelope).ok()?;
    let payload = STANDARD.decode(outer.get("payload")?.as_str()?).ok()?;
    let inner: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    Some(inner.get("schema")?.as_str()?.to_owned())
}

impl Acquisition<'_> {
    /// The set of `kind` the device holds on `channel`: its generation is the
    /// floor a new set of that channel must be above. Another channel has no
    /// floor.
    fn held_generation(&self, kind: SetKind, channel: &str) -> Result<u64> {
        let state = set_state::read_state(&self.store.system, kind)?;
        let Some(current) = state.current else {
            return Ok(0);
        };
        let (set, _) =
            set_state::load_set(&self.store.system, kind, &current, self.keys, self.arch)?;
        Ok(if set.channel == channel {
            set.generation
        } else {
            0
        })
    }

    /// Read the catalog's line of `kind` for `channel`. A newer set that does
    /// not select on `target`'s root is not offered: it waits for a system
    /// update that brings a root it selects on.
    pub fn set_check(
        &self,
        kind: SetKind,
        source: &str,
        channel: &str,
        target: &SetTarget<'_>,
    ) -> Result<VerifiedSetCatalog> {
        crate::catalog::source_url(source)?;
        self.prepare()?;
        self.reserve(crate::catalog::MAX_CATALOG_BYTES as u64)?;
        let (path, checkpoint_path, previous) = self.checkpoint()?;
        let result = crate::catalog::verify_set_catalog(
            self.keys,
            &SetCatalogRequest {
                source,
                kind,
                arch: self.arch,
                channel,
                checkpoint: previous.as_ref(),
                highest_generation: self.held_generation(kind, channel)?,
            },
            |url, limit| {
                download(url.as_str(), &path, limit, false)?;
                let bytes = read_bounded(&path, limit)?;
                fs::remove_file(&path)?;
                Ok(bytes)
            },
        )?;
        atomic_write(&checkpoint_path, &serde_json::to_vec(&result.checkpoint)?)?;
        sync_dir(path.parent().context("missing catalog parent")?)?;
        let mut result = result;
        if let Some(selected) = &result.selected {
            let refusal = match kind {
                SetKind::Core => {
                    core_set::select(&selected.set, target.features, &target.root).err()
                }
            };
            if let Some(refusal) = refusal {
                result.waits_for_system = Some(WaitingSet {
                    id: selected.set_id.clone(),
                    generation: selected.set.generation,
                    reason: refusal.to_string(),
                });
                result.selected = None;
            }
        }
        Ok(result)
    }

    /// What this device could take a set for, before any of it is fetched.
    fn set_validate(&self, set: &UpdateSet) -> Result<()> {
        ensure!(set.arch == self.arch, "set targets another architecture");
        ensure!(
            self.store.effective_state()?.candidate.is_none(),
            "a deployment is on trial; confirm it before a set update"
        );
        ensure!(
            set_state::read_state(&self.store.system, set.kind)?
                .pending
                .is_none(),
            "another set is pending"
        );
        let held = self.held_generation(set.kind, &set.channel)?;
        ensure!(set.generation > held, "set generation is not newer");
        Ok(())
    }

    /// The set's objects that are neither installed nor in the workspace.
    pub(super) fn set_missing(&self, set: &UpdateSet) -> Result<BTreeMap<String, u64>> {
        let mut missing = BTreeMap::new();
        for (place, artifact) in self.store.set_objects(set) {
            if self.store.held(&place)? {
                self.store.verify_held(&place, artifact)?;
                continue;
            }
            let stored = self.objects().join(&artifact.sha256);
            if stored.symlink_metadata().is_ok() {
                verify_file(&stored, artifact)?;
                continue;
            }
            if let Some(previous) = missing.insert(artifact.sha256.clone(), artifact.bytes) {
                ensure!(previous == artifact.bytes, "conflicting object lengths");
            }
        }
        Ok(missing)
    }

    fn set_ready(&self, envelope: &[u8], set: UpdateSet) -> Result<ReadySet> {
        ensure!(
            self.set_missing(&set)?.is_empty(),
            "set objects are incomplete"
        );
        let id = set.id()?;
        // A kind is letters and digits, so the name reads back one way.
        let path = self
            .root
            .join(format!("verified/set-{}-{id}.json", set.kind));
        atomic_write(&path, envelope)?;
        Ok(ReadySet {
            id,
            set_kind: set.kind,
            path,
            objects: self.objects(),
            channel: set.channel,
            version: set.version,
            generation: set.generation,
        })
    }

    /// Fetch the objects of a set the catalog offered.
    pub fn set_fetch(&self, selected: SelectedSet) -> Result<ReadySet> {
        self.set_validate(&selected.set)?;
        self.prepare()?;
        let missing = self.set_missing(&selected.set)?;
        self.reserve_missing(&missing)?;
        self.fetch_objects(missing, &selected.objects)?;
        self.set_ready(selected.envelope.as_bytes(), selected.set)
    }

    /// Import a MICAUPD1 archive, whichever it carries: a deployment's
    /// descriptor and objects, or a signed set and its components.
    pub fn import_any(&self, input: &mut impl Read) -> Result<Imported> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        ensure!(&magic == b"MICAUPD1", "invalid component archive");
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u64::from(u32::from_be_bytes(length));
        ensure!(
            length > 0 && length <= set_state::MAX_ENVELOPE_BYTES,
            "archive descriptor exceeds bound"
        );
        let mut envelope = vec![0; length as usize];
        input.read_exact(&mut envelope)?;
        if payload_schema(&envelope).as_deref() != Some("mica/set/v1") {
            ensure!(length <= 24576, "archive descriptor exceeds bound");
            return self
                .import_descriptor(envelope, input)
                .map(Imported::Deployment);
        }
        let set = update_set::authenticate(&envelope, self.keys)?;
        self.set_validate(&set)?;
        let mut required = BTreeMap::new();
        for artifact in set.artifacts() {
            if let Some(previous) = required.insert(artifact.sha256.clone(), artifact.bytes) {
                ensure!(previous == artifact.bytes, "conflicting object lengths");
            }
        }
        self.prepare()?;
        let missing = self.set_missing(&set)?;
        self.import_objects(input, required, &missing, false)?;
        self.set_ready(&envelope, set).map(Imported::Set)
    }
}
