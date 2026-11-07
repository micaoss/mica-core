//! Bounded online acquisition and streaming offline component archives.
use crate::{
    catalog::{
        self, CatalogCheckpoint, CatalogRequest, SelectedRelease, SourceObject, VerifiedCatalog,
    },
    chunks,
    components::{Artifact, Deployment, authenticate_deployment, component_id},
    deployments::{DeploymentStore, atomic_write, directory, read_bounded, sync_dir, verify_file},
    update_set::SetKind,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

mod extensions;
mod sets;
mod store;
mod transfer;
pub use extensions::{ReadyExtension, staged_extension_names, staged_extensions};
pub use sets::*;
use transfer::*;

/// One bounded GET of `url` into `path`, over the deployment client's TLS
/// roots, redirects and pacing. Nothing it fetches is trusted: the caller
/// checks what arrived.
///
/// # Errors
///
/// A URL other than http or https, a failed transfer, or more than `limit`
/// bytes.
pub fn download_to(url: &str, path: &Path, limit: u64) -> Result<()> {
    download(url, path, limit, false)
}

pub struct Acquisition<'a> {
    pub root: PathBuf,
    pub store: &'a DeploymentStore,
    pub keys: &'a [[u8; 32]],
    pub board: &'a str,
    pub arch: &'a str,
    pub product: &'a str,
    /// The caller's budget for the workspace; every command that writes to it
    /// needs one, and discarding does not. On a `ubi` board it also counts the
    /// objects written straight into the store.
    pub max_bytes: Option<u64>,
    /// On a `ubi` board, how a fetch retires the fallback when the objects it
    /// writes into the store would not fit otherwise ([`DeploymentStore::release_fallback`],
    /// with SYSTEM writable); without it such a fetch is refused.
    pub release: Option<&'a Release<'a>>,
}

/// How a fetch retires the fallback for the deployment it brings.
pub type Release<'a> = dyn Fn(&Deployment) -> Result<()> + 'a;
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyDeployment {
    pub id: String,
    pub path: PathBuf,
    pub objects: PathBuf,
    pub version: String,
    pub generation: u64,
}

impl Acquisition<'_> {
    pub fn objects(&self) -> PathBuf {
        self.root.join("verified/objects")
    }

    fn prepare(&self) -> Result<()> {
        for path in [
            self.root.join("downloads"),
            self.objects(),
            self.root.join("staging"),
        ] {
            directory(&path)?;
        }
        Ok(())
    }

    fn files(&self) -> Result<Vec<(PathBuf, u64)>> {
        let mut stack = vec![(self.root.clone(), 0)];
        let mut files = Vec::new();
        let mut entries = 0;
        while let Some((path, depth)) = stack.pop() {
            ensure!(depth <= 3, "workspace directory depth exceeded");
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                entries += 1;
                ensure!(entries <= 4096, "workspace entry bound exceeded");
                let meta = entry.path().symlink_metadata()?;
                if meta.is_dir() {
                    stack.push((entry.path(), depth + 1));
                } else {
                    ensure!(meta.is_file(), "unexpected workspace file type");
                    files.push((entry.path(), meta.len()));
                }
            }
        }
        Ok(files)
    }

    fn budget(&self) -> Result<u64> {
        let budget = self.max_bytes.context("no workspace budget")?;
        ensure!(
            budget > 0 && budget <= 8 * 1024 * 1024 * 1024,
            "invalid workspace budget"
        );
        Ok(budget)
    }

    fn reserve(&self, bytes: u64) -> Result<()> {
        let budget = self.budget()?;
        let used = self
            .files()?
            .into_iter()
            .try_fold(0_u64, |sum, (_, bytes)| {
                sum.checked_add(bytes).context("workspace size overflow")
            })?;
        ensure!(
            used.checked_add(bytes).is_some_and(|total| total <= budget),
            "workspace budget exhausted"
        );
        let stat = rustix::fs::statvfs(&self.root)?;
        ensure!(
            stat.f_bavail.saturating_mul(stat.f_frsize)
                >= bytes.saturating_add(self.store.reserves.data())
                && (stat.f_files == 0 || stat.f_favail >= 2048 + 32),
            "DATA reserve unavailable"
        );
        Ok(())
    }

    /// Remove only the disposable acquisition workspace under the storage lock.
    pub fn discard(&self) -> Result<usize> {
        self.prepare()?;
        let files = self.files()?;
        for (path, _) in &files {
            fs::remove_file(path)?;
            sync_dir(path.parent().context("missing workspace parent")?)?;
        }
        // With the staged descriptors gone, what a fetch wrote into the store
        // for them is held by nothing.
        Ok(files.len() + self.store.release_volumes(self.keys)?)
    }

    pub fn probe(&self) -> Result<serde_json::Value> {
        self.prepare()?;
        self.reserve(4096)?;
        let probe = self.root.join("staging/probe");
        atomic_write(&probe, b"workspace probe\n")?;
        fs::remove_file(&probe)?;
        sync_dir(probe.parent().context("missing probe parent")?)?;
        let stat = rustix::fs::statvfs(&self.root)?;
        Ok(serde_json::json!({
            "status":"ready", "root":self.root, "maxBytes":self.max_bytes,
            "freeBytes":stat.f_bavail.saturating_mul(stat.f_frsize),
            "freeInodes":stat.f_favail,
        }))
    }

    fn missing(&self, deployment: &Deployment) -> Result<BTreeMap<String, u64>> {
        catalog::artifacts(deployment)?;
        let mut missing = BTreeMap::new();
        for (place, artifact) in self.store.object_paths(deployment) {
            if self.store.held(&place)? {
                self.store.verify_held(&place, artifact)?;
            } else {
                missing.insert(artifact.sha256.clone(), artifact.bytes);
            }
        }
        for (sha, bytes) in missing.clone() {
            let path = self.objects().join(&sha);
            if path.symlink_metadata().is_ok() {
                verify_file(
                    &path,
                    &Artifact {
                        sha256: sha.clone(),
                        bytes,
                    },
                )?;
                missing.remove(&sha);
            }
        }
        Ok(missing)
    }

    fn validate(&self, deployment: &Deployment) -> Result<()> {
        ensure!(
            deployment.board == self.board && deployment.arch == self.arch,
            "deployment targets another device"
        );
        ensure!(
            deployment.product == self.product,
            "deployment targets another product"
        );
        let state = self.store.effective_state()?;
        ensure!(state.candidate.is_none(), "another deployment is pending");
        ensure!(
            deployment.generation > state.highest_generation,
            "deployment generation is not newer"
        );
        Ok(())
    }

    fn ready(&self, envelope: &[u8], deployment: Deployment) -> Result<ReadyDeployment> {
        ensure!(
            self.missing(&deployment)?.is_empty(),
            "deployment objects are incomplete"
        );
        let id = component_id(&serde_json::to_value(&deployment)?)?;
        let path = self.root.join(format!("verified/{id}.json"));
        atomic_write(&path, envelope)?;
        Ok(ReadyDeployment {
            id,
            path,
            objects: self.objects(),
            version: deployment.version,
            generation: deployment.generation,
        })
    }

    /// The staging file documents pass through, and the checkpoint of the
    /// last manifest read: the product line and the core line read the same
    /// manifest, so they share it.
    fn checkpoint(&self) -> Result<(PathBuf, PathBuf, Option<CatalogCheckpoint>)> {
        let path = self.root.join("staging/catalog.partial");
        let checkpoint_path = self.store.meta.join("catalog.json");
        let previous = if checkpoint_path.symlink_metadata().is_ok() {
            Some(serde_json::from_slice(&read_bounded(
                &checkpoint_path,
                4096,
            )?)?)
        } else {
            None
        };
        Ok((path, checkpoint_path, previous))
    }

    pub fn check(&self, source: &str) -> Result<VerifiedCatalog> {
        catalog::source_url(source)?;
        self.prepare()?;
        self.reserve(catalog::MAX_CATALOG_BYTES as u64)?;
        let (path, checkpoint_path, previous) = self.checkpoint()?;
        // One document at a time through the same staging file: the
        // manifest, then only for a newer release its document and descriptor.
        let result = catalog::verify_catalog(
            self.keys,
            &CatalogRequest {
                source,
                board: self.board,
                arch: self.arch,
                product: self.product,
                checkpoint: previous.as_ref(),
                highest_generation: self.store.effective_state()?.highest_generation,
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

    fn reserve_missing(&self, missing: &BTreeMap<String, u64>) -> Result<()> {
        let mut needed = 0_u64;
        for (sha, bytes) in missing {
            let partial = self.root.join(format!("downloads/{sha}.partial"));
            let held = if partial.symlink_metadata().is_ok() {
                let meta = partial.symlink_metadata()?;
                ensure!(
                    meta.is_file() && meta.len() <= *bytes,
                    "invalid partial object"
                );
                meta.len()
            } else {
                0
            };
            needed = needed
                .checked_add(bytes - held)
                .context("object size overflow")?;
        }
        self.reserve(needed + 24576)
    }

    /// Every file on this device that a chunk may be copied out of: the objects
    /// of the installed deployments, the components of every core set the
    /// state names, and whatever earlier acquisition left in the object store.
    /// A seed is read, never trusted -- each chunk taken from
    /// one is checked against the digest that named it, and the assembled
    /// object against the signed digest -- so a seed that cannot be read or
    /// authenticated is skipped rather than refused.
    fn seeds(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for entry in self.store.entries().unwrap_or_default() {
            let Ok(bytes) = read_bounded(
                &self
                    .store
                    .system
                    .join(format!("deployments/{}.json", entry.id)),
                24576,
            ) else {
                continue;
            };
            let Ok(deployment) = authenticate_deployment(&bytes, self.keys) else {
                continue;
            };
            paths.extend(
                self.store
                    .object_paths(&deployment)
                    .into_iter()
                    .filter_map(|(place, _)| self.store.readable(&place).ok().flatten()),
            );
        }
        for set in self.store.held_sets(SetKind::Core, self.keys) {
            paths.extend(
                self.store
                    .set_objects(&set)
                    .into_iter()
                    .filter_map(|(place, _)| self.store.readable(&place).ok().flatten()),
            );
        }
        if let Ok(stored) = fs::read_dir(self.objects()) {
            paths.extend(stored.flatten().map(|entry| entry.path()));
        }
        paths.sort();
        paths.dedup();
        paths
    }

    /// Assemble one object from chunks this device already holds plus the ones
    /// it does not. The index and the chunks are unsigned and unauthenticated:
    /// what makes this safe is that the result goes through the same promotion
    /// a whole download does, so the worst a hostile index achieves is wasted
    /// bandwidth. Any failure here is an ordinary `Err` and the caller falls
    /// back to fetching the whole object.
    fn delta(&self, object: &SourceObject, partial: &Path, seeds: &chunks::SeedMap) -> Result<()> {
        // Assembled beside the partial, never into it: a partial is a resumable
        // transfer, and truncating one to try a delta would throw away bytes
        // that are already here if the delta then failed.
        let assembly = self
            .root
            .join(format!("downloads/{}.assembly", object.sha256));
        let mut output = File::create(&assembly)?;
        self.assemble(object, seeds, &mut output)?;
        output.sync_all()?;
        fs::rename(&assembly, partial)?;
        Ok(())
    }

    /// Write `object` into `output` from its index: each chunk from a seed
    /// that holds it, else from the origin's chunk store, checked against the
    /// index either way.
    fn assemble(
        &self,
        object: &SourceObject,
        seeds: &chunks::SeedMap,
        output: &mut dyn Write,
    ) -> Result<()> {
        let artifact = Artifact {
            sha256: object.sha256.clone(),
            bytes: object.bytes,
        };
        let limit = chunks::index_limit(object.bytes);
        self.reserve(limit + chunks::MAX_CHUNK)?;
        let index_path = self.root.join(format!("downloads/{}.index", object.sha256));
        // The index is the object's path with `.index`, and the chunks sit in
        // `chunks/` beside the object's directory, at the same store.
        let source = url::Url::parse(&object.url)?;
        let mut index_url = source.clone();
        index_url.set_query(None);
        index_url.set_path(&format!("{}.index", source.path()));
        download(index_url.as_str(), &index_path, limit, false)?;
        let index = chunks::parse_index(&read_bounded(&index_path, limit)?, &artifact)?;
        fs::remove_file(&index_path)?;
        let chunk_path = self.root.join(format!("downloads/{}.chunk", object.sha256));
        for (sha, length) in index.0 {
            let bytes = match seeds.get(&sha) {
                Some((path, offset, held)) if *held == length => {
                    chunks::read_seed(path, *offset, length, &sha)?
                }
                _ => {
                    let url = source.join(&format!("../chunks/{sha}"))?;
                    download(url.as_str(), &chunk_path, length, false)?;
                    verify_file(
                        &chunk_path,
                        &Artifact {
                            sha256: sha.clone(),
                            bytes: length,
                        },
                    )?;
                    read_bounded(&chunk_path, length)?
                }
            };
            output.write_all(&bytes)?;
        }
        let _ = fs::remove_file(&chunk_path);
        Ok(())
    }

    /// What a failed delta of the object `sha` leaves in the workspace.
    fn drop_delta(&self, sha: &str) {
        for leftover in ["assembly", "index", "chunk"] {
            let _ = fs::remove_file(self.root.join(format!("downloads/{sha}.{leftover}")));
        }
    }

    pub fn fetch(&self, selected: SelectedRelease) -> Result<ReadyDeployment> {
        let deployment = &selected.deployment;
        self.validate(deployment)?;
        self.prepare()?;
        // The seeds are cut once for the whole fetch, the set's objects
        // included: re-reading them per call would multiply the slowest step.
        let mut seeds = None;
        let none = BTreeMap::new();
        // The image's set, when installing the deployment would take it: its
        // envelope first, which names the components fetched with the
        // deployment's objects.
        let set = match self
            .store
            .newer_image_set(deployment, self.keys, self.arch)?
        {
            Some(image) => {
                let envelope = BTreeMap::from([(image.sha256.clone(), image.bytes)]);
                let missing = self.absent(envelope)?;
                self.reserve_missing(&missing)?;
                self.fetch_objects(missing, &selected.objects, &mut seeds, &none)?;
                Some(
                    self.store
                        .image_set(image, &self.objects(), self.keys, self.arch)?
                        .0,
                )
            }
            None => None,
        };
        let mut places = self.store.object_paths(deployment);
        if let Some(set) = &set {
            places.extend(self.store.set_objects(set));
        }
        let volumes = self.volumes(places);
        let missing = self.missing(deployment)?;
        let set_missing = match &set {
            Some(set) => self.set_missing(set)?,
            None => BTreeMap::new(),
        };
        let mut all = missing.clone();
        all.extend(set_missing.clone());
        self.room(&all, &volumes, Some(deployment))?;
        self.fetch_objects(missing, &selected.objects, &mut seeds, &volumes)?;
        self.fetch_objects(set_missing, &selected.objects, &mut seeds, &volumes)?;
        self.ready(selected.envelope.as_bytes(), selected.deployment)
    }

    /// The objects of `wanted` that are not in the verified store.
    fn absent(&self, mut wanted: BTreeMap<String, u64>) -> Result<BTreeMap<String, u64>> {
        for (sha, bytes) in wanted.clone() {
            let path = self.objects().join(&sha);
            if path.symlink_metadata().is_ok() {
                verify_file(
                    &path,
                    &Artifact {
                        sha256: sha.clone(),
                        bytes,
                    },
                )?;
                wanted.remove(&sha);
            }
        }
        Ok(wanted)
    }

    /// Fetch every object of `missing` from where `objects` says it is, by
    /// delta where the origin and this device allow, smallest first, and
    /// promote each into the verified store, or write it straight into its
    /// volume when `volumes` names one.
    /// `seeds` is the caller's cut, made on first need and reused by every
    /// later call of the same fetch.
    fn fetch_objects(
        &self,
        missing: BTreeMap<String, u64>,
        objects: &[SourceObject],
        seeds: &mut Option<chunks::SeedMap>,
        volumes: &BTreeMap<String, Vec<String>>,
    ) -> Result<()> {
        // One probe per fetch, not one per object: an origin that did not serve
        // the first index is not expected to serve the next, and being wrong
        // about that costs exactly what this repository did before deltas --
        // the whole object.
        let mut origin_has_indexes = true;
        let mut missing: Vec<_> = missing.into_iter().collect();
        // The largest object last: a power cut loses the object in flight.
        missing.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        for (sha, bytes) in missing {
            let object = objects
                .iter()
                .find(|object| object.sha256 == sha && object.bytes == bytes)
                .context("missing acquisition URL")?;
            if let Some(names) = volumes.get(&sha) {
                self.fetch_into(object, names, seeds, &mut origin_has_indexes)?;
                continue;
            }
            let partial = self.root.join(format!("downloads/{sha}.partial"));
            let held = if partial.try_exists()? {
                partial.metadata()?.len()
            } else {
                0
            };
            // An interrupted transfer is resumed rather than restarted as a
            // delta: the bytes on disk are already paid for.
            if held == 0 && origin_has_indexes {
                let cut = match seeds {
                    Some(cut) => cut,
                    None => seeds.insert(chunks::seed_map(&self.seeds())?),
                };
                if self.delta(object, &partial, cut).is_err() {
                    origin_has_indexes = false;
                    self.drop_delta(&sha);
                }
            }
            if !partial.try_exists()? || partial.metadata()?.len() < bytes {
                download(&object.url, &partial, bytes, true)?;
            }
            self.promote(&partial, &Artifact { sha256: sha, bytes })?;
        }
        Ok(())
    }

    fn promote(&self, partial: &Path, artifact: &Artifact) -> Result<()> {
        if let Err(error) = verify_file(partial, artifact) {
            fs::remove_file(partial)?;
            sync_dir(partial.parent().context("missing object parent")?)?;
            return Err(error);
        }
        File::open(partial)?.sync_all()?;
        fs::rename(partial, self.objects().join(&artifact.sha256))?;
        sync_dir(&self.objects())?;
        Ok(sync_dir(
            partial.parent().context("missing object parent")?,
        )?)
    }

    /// MICAUPD1: descriptor length (u32 BE), signed descriptor, object count
    /// (u32 BE), then digest (64 ASCII), size (u64 BE), and exact object bytes.
    /// There are no filenames, directory entries, links, compression or padding.
    /// An archive may carry any subset of the descriptor's objects (a root- or
    /// kernel-only update); every object it does not carry must already be present.
    pub fn import(&self, input: &mut impl Read) -> Result<ReadyDeployment> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        ensure!(&magic == b"MICAUPD1", "invalid component archive");
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        ensure!(
            length > 0 && length <= 24576,
            "archive descriptor exceeds bound"
        );
        let mut envelope = vec![0; length];
        input.read_exact(&mut envelope)?;
        self.import_descriptor(envelope, input)
    }

    /// The rest of an archive whose envelope is a deployment's descriptor.
    fn import_descriptor(
        &self,
        envelope: Vec<u8>,
        input: &mut impl Read,
    ) -> Result<ReadyDeployment> {
        let deployment = authenticate_deployment(&envelope, self.keys)?;
        self.validate(&deployment)?;
        let mut required = catalog::artifacts(&deployment)?;
        self.prepare()?;
        let mut missing = self.missing(&deployment)?;
        // The image's set travels with its deployment: its envelope, and the
        // objects of its components, which the envelope names.
        let image = deployment.core_set.clone();
        if let Some(image) = &image {
            let envelope = BTreeMap::from([(image.sha256.clone(), image.bytes)]);
            missing.extend(self.absent(envelope.clone())?);
            for (sha, bytes) in envelope {
                ensure!(
                    *required.entry(sha).or_insert(bytes) == bytes,
                    "conflicting object lengths"
                );
            }
        }
        // The image's components stay staged: which volume each goes to is
        // known only once its set's envelope, in the same archive, is read.
        let volumes = self.volumes(self.store.object_paths(&deployment));
        self.room(&missing, &volumes, Some(&deployment))?;
        let extras = self.import_objects(input, required, &missing, image.is_some(), &volumes)?;
        if let Some(image) = &image {
            let named = match self
                .store
                .image_set(image, &self.objects(), self.keys, self.arch)
            {
                Ok((set, _)) => set
                    .artifacts()
                    .map(|artifact| (artifact.sha256.clone(), artifact.bytes))
                    .collect(),
                Err(_) => BTreeMap::new(),
            };
            if extras
                .iter()
                .any(|(sha, bytes)| named.get(sha) != Some(bytes))
            {
                for sha in extras.keys() {
                    let _ = fs::remove_file(self.objects().join(sha));
                }
                sync_dir(&self.objects())?;
                anyhow::bail!("archive carries objects its core set does not name");
            }
            if let Some(image) = self
                .store
                .newer_image_set(&deployment, self.keys, self.arch)?
            {
                let (set, _) =
                    self.store
                        .image_set(image, &self.objects(), self.keys, self.arch)?;
                ensure!(
                    self.set_missing(&set)?.is_empty(),
                    "set objects are incomplete"
                );
            }
        }
        self.ready(&envelope, deployment)
    }

    /// The object section of an archive: a count, then each object's digest,
    /// size and bytes. Every object must be one `required` names, once; the
    /// ones in `missing` are written and promoted, or written straight into
    /// the volume `volumes` names, the rest only checked. The caller has made
    /// [`Self::room`] for `missing`.
    /// With `extras`, an object `required` does not name is taken too, once
    /// and within the budget, and returned for the caller to hold to what
    /// names it.
    fn import_objects(
        &self,
        input: &mut impl Read,
        mut required: BTreeMap<String, u64>,
        missing: &BTreeMap<String, u64>,
        extras: bool,
        volumes: &BTreeMap<String, Vec<String>>,
    ) -> Result<BTreeMap<String, u64>> {
        let mut count = [0; 4];
        input.read_exact(&mut count)?;
        let count = u32::from_be_bytes(count) as usize;
        ensure!(
            count <= required.len() || (extras && count <= catalog::MAX_ENTRIES),
            "archive carries more objects than its descriptor names"
        );
        let mut taken = BTreeMap::new();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..count {
            let mut sha = [0; 64];
            input.read_exact(&mut sha)?;
            let sha = String::from_utf8(sha.to_vec())?;
            let mut size = [0; 8];
            input.read_exact(&mut size)?;
            let size = u64::from_be_bytes(size);
            let extra = required.remove(&sha) != Some(size);
            ensure!(
                seen.insert(sha.clone())
                    && (!extra
                        || (extras
                            && catalog::sha256_hex(&sha)
                            && size > 0
                            && !missing.contains_key(&sha))),
                "unlisted, duplicate or wrong-sized archive object"
            );
            let write = if extra {
                let path = self.objects().join(&sha);
                let held = path.symlink_metadata().is_ok()
                    && verify_file(
                        &path,
                        &Artifact {
                            sha256: sha.clone(),
                            bytes: size,
                        },
                    )
                    .is_ok();
                if !held {
                    self.reserve(size)?;
                    taken.insert(sha.clone(), size);
                }
                !held
            } else {
                missing.contains_key(&sha)
            };
            let artifact = Artifact {
                sha256: sha.clone(),
                bytes: size,
            };
            if write && let Some(names) = volumes.get(&sha) {
                self.import_into(input, names, &artifact)?;
                continue;
            }
            let partial = self.root.join(format!("downloads/{sha}.partial"));
            let mut output = if write {
                if partial.symlink_metadata().is_ok() {
                    ensure!(
                        partial.symlink_metadata()?.is_file(),
                        "invalid partial object"
                    );
                    fs::remove_file(&partial)?;
                }
                Some(
                    OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&partial)?,
                )
            } else {
                None
            };
            store::copy_checked(
                input,
                &artifact,
                output.as_mut().map(|output| output as &mut dyn Write),
            )?;
            if let Some(output) = output {
                output.sync_all()?;
                drop(output);
                self.promote(&partial, &artifact)?;
            }
        }
        ensure!(input.read(&mut [0; 1])? == 0, "trailing archive bytes");
        Ok(taken)
    }
}
