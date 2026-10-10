//! Acquiring a core set: the catalog's core line, and a core archive.
//!
//! The same workspace, budget, object store and delta transfer as a
//! deployment's. What is left ready is the signed set and its objects, for
//! `mica-deploy core-install`.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use super::*;
use crate::catalog::{CoreCatalogRequest, SelectedCoreSet, VerifiedCoreCatalog};
use crate::core_set::{self, CoreSet};
use crate::core_state;

/// A core set whose signed envelope and every object are in the workspace.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyCoreSet {
    pub id: String,
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
    CoreSet(ReadyCoreSet),
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
    /// The set the device holds on `channel`: its generation is the floor a
    /// new set of that channel must be above. Another channel has no floor.
    fn held_generation(&self, channel: &str) -> Result<u64> {
        let state = core_state::read_state(&self.store.system)?;
        let Some(current) = state.current else {
            return Ok(0);
        };
        let (set, _) = core_state::load_set(&self.store.system, &current, self.keys, self.arch)?;
        Ok(if set.channel == channel {
            set.generation
        } else {
            0
        })
    }

    /// Read the catalog's core line for `channel`.
    pub fn core_check(&self, source: &str, channel: &str) -> Result<VerifiedCoreCatalog> {
        crate::catalog::source_url(source)?;
        self.prepare()?;
        self.reserve(crate::catalog::MAX_CATALOG_BYTES as u64)?;
        let (path, checkpoint_path, previous) = self.checkpoint()?;
        let result = crate::catalog::verify_core_catalog(
            self.keys,
            &CoreCatalogRequest {
                source,
                arch: self.arch,
                channel,
                checkpoint: previous.as_ref(),
                highest_generation: self.held_generation(channel)?,
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
        Ok(result)
    }

    /// What this device could take a set for, before any of it is fetched.
    fn core_validate(&self, set: &CoreSet) -> Result<()> {
        ensure!(
            set.arch == self.arch,
            "core set targets another architecture"
        );
        ensure!(
            self.store.effective_state()?.candidate.is_none(),
            "a deployment is on trial; confirm it before a core update"
        );
        ensure!(
            core_state::read_state(&self.store.system)?
                .pending
                .is_none(),
            "another core set is pending"
        );
        let held = self.held_generation(&set.channel)?;
        ensure!(set.generation > held, "core set generation is not newer");
        Ok(())
    }

    /// The set's objects that are neither installed nor in the workspace.
    fn core_missing(&self, set: &CoreSet) -> Result<BTreeMap<String, u64>> {
        let mut missing = BTreeMap::new();
        for (path, artifact) in self.store.core_objects(set) {
            if path.symlink_metadata().is_ok() {
                verify_file(&path, artifact)?;
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

    fn core_ready(&self, envelope: &[u8], set: CoreSet) -> Result<ReadyCoreSet> {
        ensure!(
            self.core_missing(&set)?.is_empty(),
            "core set objects are incomplete"
        );
        let id = set.id()?;
        let path = self.root.join(format!("verified/core-{id}.json"));
        atomic_write(&path, envelope)?;
        Ok(ReadyCoreSet {
            id,
            path,
            objects: self.objects(),
            channel: set.channel,
            version: set.version,
            generation: set.generation,
        })
    }

    /// Fetch the objects of a set the catalog offered.
    pub fn core_fetch(&self, selected: SelectedCoreSet) -> Result<ReadyCoreSet> {
        self.core_validate(&selected.set)?;
        self.prepare()?;
        let missing = self.core_missing(&selected.set)?;
        self.reserve_missing(&missing)?;
        self.fetch_objects(missing, &selected.objects)?;
        self.core_ready(selected.envelope.as_bytes(), selected.set)
    }

    /// Import a MICAUPD1 archive, whichever it carries: a deployment's
    /// descriptor and objects, or a signed core set and its components.
    pub fn import_any(&self, input: &mut impl Read) -> Result<Imported> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        ensure!(&magic == b"MICAUPD1", "invalid component archive");
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u64::from(u32::from_be_bytes(length));
        ensure!(
            length > 0 && length <= core_state::MAX_ENVELOPE_BYTES,
            "archive descriptor exceeds bound"
        );
        let mut envelope = vec![0; length as usize];
        input.read_exact(&mut envelope)?;
        if payload_schema(&envelope).as_deref() != Some("mica/core-set/v1") {
            ensure!(length <= 24576, "archive descriptor exceeds bound");
            return self
                .import_descriptor(envelope, input)
                .map(Imported::Deployment);
        }
        let set = core_set::authenticate(&envelope, self.keys)?;
        self.core_validate(&set)?;
        let mut required = BTreeMap::new();
        for artifact in set.artifacts() {
            if let Some(previous) = required.insert(artifact.sha256.clone(), artifact.bytes) {
                ensure!(previous == artifact.bytes, "conflicting object lengths");
            }
        }
        self.prepare()?;
        let missing = self.core_missing(&set)?;
        self.import_objects(input, required, &missing)?;
        self.core_ready(&envelope, set).map(Imported::CoreSet)
    }
}
