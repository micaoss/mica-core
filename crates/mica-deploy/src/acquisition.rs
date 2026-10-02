//! Bounded online acquisition and streaming offline component archives.
use crate::{
    catalog::{
        self, CatalogCheckpoint, CatalogRequest, SelectedRelease, SourceObject, VerifiedCatalog,
    },
    chunks,
    components::{Artifact, Deployment, authenticate_deployment, component_id},
    deployments::{DeploymentStore, atomic_write, directory, read_bounded, sync_dir, verify_file},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

mod transfer;
use transfer::*;

pub struct Acquisition<'a> {
    pub root: PathBuf,
    pub store: &'a DeploymentStore,
    pub keys: &'a [[u8; 32]],
    pub board: &'a str,
    pub arch: &'a str,
    pub product: &'a str,
    /// The caller's budget for the workspace; every command that writes to it
    /// needs one, and discarding does not.
    pub max_bytes: Option<u64>,
}
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
        Ok(files.len())
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
        for (path, artifact) in self.store.object_paths(deployment) {
            if path.symlink_metadata().is_ok() {
                verify_file(&path, artifact)?;
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

    pub fn check(&self, source: &str) -> Result<VerifiedCatalog> {
        catalog::source_url(source)?;
        self.prepare()?;
        self.reserve(catalog::MAX_CATALOG_BYTES as u64)?;
        let path = self.root.join("staging/catalog.partial");
        let checkpoint_path = self.store.meta.join("catalog.json");
        let previous: Option<CatalogCheckpoint> = if checkpoint_path.symlink_metadata().is_ok() {
            Some(serde_json::from_slice(&read_bounded(
                &checkpoint_path,
                4096,
            )?)?)
        } else {
            None
        };
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
    /// of the installed deployments, and whatever earlier acquisition left in
    /// the object store. A seed is read, never trusted -- each chunk taken from
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
                    .map(|(path, _)| path),
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
    fn delta(
        &self,
        object: &SourceObject,
        partial: &Path,
        seeds: &BTreeMap<String, (PathBuf, u64, u64)>,
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
        // Assembled beside the partial, never into it: a partial is a resumable
        // transfer, and truncating one to try a delta would throw away bytes
        // that are already here if the delta then failed.
        let assembly = self
            .root
            .join(format!("downloads/{}.assembly", object.sha256));
        let mut output = File::create(&assembly)?;
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
        output.sync_all()?;
        let _ = fs::remove_file(&chunk_path);
        fs::rename(&assembly, partial)?;
        Ok(())
    }

    pub fn fetch(&self, selected: SelectedRelease) -> Result<ReadyDeployment> {
        self.validate(&selected.deployment)?;
        self.prepare()?;
        let missing = self.missing(&selected.deployment)?;
        self.reserve_missing(&missing)?;
        let mut seeds = None;
        // One probe per fetch, not one per object: an origin that did not serve
        // the first index is not expected to serve the next, and being wrong
        // about that costs exactly what this repository did before deltas --
        // the whole object.
        let mut origin_has_indexes = true;
        for (sha, bytes) in missing {
            let object = selected
                .objects
                .iter()
                .find(|object| object.sha256 == sha && object.bytes == bytes)
                .context("missing acquisition URL")?;
            let partial = self.root.join(format!("downloads/{sha}.partial"));
            let held = if partial.try_exists()? {
                partial.metadata()?.len()
            } else {
                0
            };
            // An interrupted transfer is resumed rather than restarted as a
            // delta: the bytes on disk are already paid for.
            if held == 0 && origin_has_indexes {
                // The seeds are cut once and reused for every object of this
                // deployment: re-reading them per object would multiply the
                // slowest step by the number of objects for nothing.
                let cut = match &seeds {
                    Some(cut) => cut,
                    None => seeds.insert(chunks::seed_map(&self.seeds())?),
                };
                if self.delta(object, &partial, cut).is_err() {
                    origin_has_indexes = false;
                    for leftover in [
                        self.root.join(format!("downloads/{sha}.assembly")),
                        self.root.join(format!("downloads/{sha}.index")),
                        self.root.join(format!("downloads/{sha}.chunk")),
                    ] {
                        let _ = fs::remove_file(leftover);
                    }
                }
            }
            if !partial.try_exists()? || partial.metadata()?.len() < bytes {
                download(&object.url, &partial, bytes, true)?;
            }
            self.promote(&partial, &Artifact { sha256: sha, bytes })?;
        }
        self.ready(selected.envelope.as_bytes(), selected.deployment)
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
        let deployment = authenticate_deployment(&envelope, self.keys)?;
        self.validate(&deployment)?;
        let mut required = catalog::artifacts(&deployment)?;
        let mut count = [0; 4];
        input.read_exact(&mut count)?;
        let count = u32::from_be_bytes(count) as usize;
        ensure!(
            count <= required.len(),
            "archive carries more objects than the deployment names"
        );
        self.prepare()?;
        let missing = self.missing(&deployment)?;
        self.reserve_missing(&missing)?;
        for _ in 0..count {
            let mut sha = [0; 64];
            input.read_exact(&mut sha)?;
            let sha = String::from_utf8(sha.to_vec())?;
            let mut size = [0; 8];
            input.read_exact(&mut size)?;
            let size = u64::from_be_bytes(size);
            ensure!(
                required.remove(&sha) == Some(size),
                "unlisted, duplicate or wrong-sized archive object"
            );
            let partial = self.root.join(format!("downloads/{sha}.partial"));
            let mut output = if missing.contains_key(&sha) {
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
            let mut remaining = size;
            let mut buffer = [0; 65536];
            let mut hash = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
            while remaining > 0 {
                let count = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..count])?;
                hash.update(&buffer[..count]);
                if let Some(output) = &mut output {
                    output.write_all(&buffer[..count])?;
                }
                remaining -= count as u64;
            }
            ensure!(
                hex::encode(hash.finish()) == sha,
                "archive object digest mismatch"
            );
            if let Some(output) = output {
                output.sync_all()?;
                drop(output);
                self.promote(
                    &partial,
                    &Artifact {
                        sha256: sha,
                        bytes: size,
                    },
                )?;
            }
        }
        ensure!(input.read(&mut [0; 1])? == 0, "trailing archive bytes");
        self.ready(&envelope, deployment)
    }
}
