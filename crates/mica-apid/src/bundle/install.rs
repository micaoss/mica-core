//! Staging, installing, activating, deactivating and deleting bundles.

use anyhow::{Context, bail};
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, symlink};

use super::*;

impl Store {
    /// Generations with a staged tree waiting to be activated, ascending.
    pub fn discover_staged(&self) -> anyhow::Result<Vec<u64>> {
        let mut found = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(found),
            Err(err) => {
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("read dir {}", self.root.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(generation) = name
                .to_str()
                .and_then(|name| name.strip_prefix(".staging-"))
                .and_then(|digits| digits.parse::<u64>().ok())
            else {
                continue;
            };
            found.push(generation);
        }
        found.sort_unstable();
        Ok(found)
    }

    /// The second sanctioned local install path: apid picks up a
    /// staged directory. Activates the highest staged generation and returns
    /// `None` when there is nothing staged.
    ///
    /// `served` is the served set. It is an input because version negotiation is
    /// not implemented: the set belongs to `GET /api/versions` and this module
    /// must not invent it.
    pub fn pick_up_staged(&self, served: &[&str]) -> anyhow::Result<Option<Activation>> {
        let Some(generation) = self.discover_staged()?.pop() else {
            return Ok(None);
        };
        self.activate(generation, served).map(Some)
    }

    /// Install `.staging-<generation>` without changing the active pointer.
    ///
    /// Validation runs on the staged tree and never on the live one, so every
    /// refusal leaves the previously active bundle active and untouched.
    ///
    /// # Errors
    ///
    /// Returns a downcastable [`Rejection`] when the staged tree is refused,
    /// and an I/O error otherwise. In both cases nothing an operator can see
    /// has changed.
    pub(super) fn install_staged(
        &self,
        generation: u64,
        served: &[&str],
        enforce_upload_policy: bool,
        package_sizes: Option<(u64, u64)>,
    ) -> anyhow::Result<Activation> {
        let staging = self.staging_dir(generation);

        // Step 1 is the operator's or the upload path's: the tree is already
        // at `staging`, which is under the root and therefore on the same
        // filesystem as the target, so step 3's rename is atomic.
        let staged_meta = fs::symlink_metadata(&staging)
            .with_context(|| format!("stat {}", staging.display()))
            .map_err(|_| Rejection::StagingNotDirectory(staging.clone()))?;
        if !staged_meta.is_dir() {
            bail!(Rejection::StagingNotDirectory(staging));
        }
        let target = self.bundle_dir(generation);
        if fs::symlink_metadata(&target).is_ok() {
            bail!(Rejection::GenerationExists(generation));
        }
        let generations = self.generations()?;
        if enforce_upload_policy && generations.len() >= 32 {
            bail!(Rejection::RetentionLimit(32));
        }

        // Step 2: validate the staged tree, completely.
        let entries = validate_tree(&staging)?;
        let manifest = read_manifest(&staging)?;
        let compat = check_compat(manifest.as_ref(), served)?;

        apply_modes(&staging, &entries)?;
        let digest = digest_tree(&staging, &entries)?;

        for installed in generations {
            let installed_dir = self.bundle_dir(installed);
            let identity_conflict = if enforce_upload_policy {
                match (manifest.as_ref(), read_manifest(&installed_dir)) {
                    (Some(new), Ok(Some(old))) => {
                        new.name == old.name && new.version == old.version
                    }
                    _ => false,
                }
            } else {
                false
            };
            let installed_entries = match validate_tree(&installed_dir) {
                Ok(entries) => entries,
                Err(_) if !identity_conflict => continue,
                Err(_) => {
                    let new = manifest.as_ref().context("missing upload manifest")?;
                    bail!(Rejection::NameVersionConflict {
                        name: new.name.clone(),
                        version: new.version.clone(),
                        generation: installed,
                    });
                }
            };
            let installed_digest = match digest_tree(&installed_dir, &installed_entries) {
                Ok(digest) => digest,
                Err(_) if !identity_conflict => continue,
                Err(_) => {
                    let new = manifest.as_ref().context("missing upload manifest")?;
                    bail!(Rejection::NameVersionConflict {
                        name: new.name.clone(),
                        version: new.version.clone(),
                        generation: installed,
                    });
                }
            };
            if enforce_upload_policy && installed_digest == digest {
                bail!(Rejection::DuplicatePackage(installed));
            }
            if identity_conflict {
                let new = manifest.as_ref().context("missing upload manifest")?;
                bail!(Rejection::NameVersionConflict {
                    name: new.name.clone(),
                    version: new.version.clone(),
                    generation: installed,
                });
            }
        }

        // Step 3: fsync the staged tree and its parent, then rename.
        fsync_tree(&staging, &entries)?;
        self.ensure_layout()?;
        fsync_dir(&self.root)?;
        fs::rename(&staging, &target)
            .with_context(|| format!("rename {} to {}", staging.display(), target.display()))?;
        fsync_dir(&self.bundles_dir())?;
        self.write_record(
            generation,
            &Record {
                digest: digest.clone(),
                compat: compat.clone(),
                compressed_bytes: package_sizes.map(|sizes| sizes.0),
                expanded_bytes: package_sizes.map(|sizes| sizes.1),
            },
        )?;

        Ok(Activation {
            generation,
            digest,
            compat,
            manifest,
        })
    }

    /// Install an uploaded staged tree without changing the active pointer.
    pub fn install(
        &self,
        generation: u64,
        served: &[&str],
        compressed_bytes: u64,
        expanded_bytes: u64,
    ) -> anyhow::Result<Activation> {
        self.install_staged(
            generation,
            served,
            true,
            Some((compressed_bytes, expanded_bytes)),
        )
    }

    /// Install and activate a manually staged tree.
    pub fn activate(&self, generation: u64, served: &[&str]) -> anyhow::Result<Activation> {
        let activation = self.install_staged(generation, served, false, None)?;
        self.point_current_at(generation)?;
        Ok(activation)
    }

    /// The deactivate, which is also the escape: remove `current`. The
    /// bundle stays on disk. Returns whether a pointer was there to remove.
    pub fn deactivate(&self) -> anyhow::Result<bool> {
        let link = self.current_link();
        match fs::remove_file(&link) {
            Ok(()) => {
                fsync_dir(&self.root)?;
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => {
                Err(anyhow::Error::new(err)).with_context(|| format!("remove {}", link.display()))
            }
        }
    }

    /// The delete: rename the tree to `.trash-<generation>`, then unlink it
    /// recursively.
    ///
    /// # Errors
    ///
    /// Refuses with [`Rejection::DeleteWhileActive`] when `current` points at
    /// this generation. Deactivate first.
    pub fn delete(&self, generation: u64) -> anyhow::Result<()> {
        if self.active_generation()? == Some(generation) {
            bail!(Rejection::DeleteWhileActive(generation));
        }
        let dir = self.bundle_dir(generation);
        if fs::symlink_metadata(&dir).is_err() {
            return Ok(());
        }
        let trash = self.trash_dir(generation);
        if fs::symlink_metadata(&trash).is_ok() {
            fs::remove_dir_all(&trash).with_context(|| format!("remove {}", trash.display()))?;
        }
        fs::rename(&dir, &trash)
            .with_context(|| format!("rename {} to {}", dir.display(), trash.display()))?;
        fs::remove_dir_all(&trash).with_context(|| format!("remove {}", trash.display()))?;
        let record = self.record_path(generation);
        match fs::remove_file(&record) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("remove {}", record.display()));
            }
        }
        Ok(())
    }

    /// "What is installed right now?", answered from the served tree.
    ///
    /// An absent root, an absent `current` and an unresolvable `current` are
    /// all [`Installed::BuiltIn`] — a named answer, never an error and never
    /// an empty field.
    pub fn status(&self) -> anyhow::Result<Installed> {
        let Some(generation) = self.active_generation()? else {
            return Ok(Installed::BuiltIn);
        };
        let dir = self.bundle_dir(generation);
        let manifest = read_manifest(&dir).ok().flatten().map(|m| ManifestSummary {
            name: m.name,
            version: m.version,
        });
        let index = dir.join(INDEX_NAME);
        let index_readable =
            fs::symlink_metadata(&index).is_ok_and(|m| m.is_file()) && File::open(&index).is_ok();
        let recorded = self.read_record(generation)?.map(|record| RecordedState {
            digest_matches: digest_of(&dir).is_ok_and(|digest| digest == record.digest),
            digest: record.digest,
            compat: record.compat,
        });
        Ok(Installed::Custom(CustomUi {
            generation,
            manifest,
            index_readable,
            recorded,
        }))
    }

    /// Re-evaluate the active bundle against a served set: class 3's
    /// digest re-check and class 5's compatibility re-check, which run at
    /// activation and at apid start-up and **not** per request.
    ///
    /// Returns `None` when no bundle is active. Never deactivates anything —
    /// the caller owns that decision and the log line.
    pub fn recheck_active(&self, served: &[&str]) -> anyhow::Result<Option<Recheck>> {
        let Some(generation) = self.active_generation()? else {
            return Ok(None);
        };
        let dir = self.bundle_dir(generation);
        let digest_matches = self
            .read_record(generation)?
            .map(|record| digest_of(&dir).is_ok_and(|digest| digest == record.digest));
        let manifest = read_manifest(&dir).ok().flatten();
        let compat = match check_compat(manifest.as_ref(), served) {
            Ok(compat) => compat,
            Err(err) => match err.downcast_ref::<Rejection>() {
                Some(Rejection::Incompatible { declared, served }) => CompatCheck::Ran {
                    declared: declared.clone(),
                    served: served.clone(),
                    compatible: false,
                },
                _ => return Err(err),
            },
        };
        Ok(Some(Recheck {
            generation,
            digest_matches,
            compat,
        }))
    }

    /// Create the root, `bundles/` and `records/` with mode 0755. Called from
    /// the install path only: nothing creates `/mica/ui` at start-up.
    pub(super) fn ensure_layout(&self) -> anyhow::Result<()> {
        for dir in [self.root.clone(), self.bundles_dir(), self.records_dir()] {
            if !dir.exists() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(DIR_MODE)
                    .create(&dir)
                    .with_context(|| format!("create {}", dir.display()))?;
            }
        }
        Ok(())
    }

    pub(super) fn point_current_at(&self, generation: u64) -> anyhow::Result<()> {
        let staged_link = self.root.join(format!(".current-{generation}.tmp"));
        match fs::remove_file(&staged_link) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("remove {}", staged_link.display()));
            }
        }
        // A relative target, so the pointer stays correct however the root is
        // reached.
        symlink(format!("bundles/{generation}"), &staged_link)
            .with_context(|| format!("symlink {}", staged_link.display()))?;
        let link = self.current_link();
        fs::rename(&staged_link, &link)
            .with_context(|| format!("rename {} to {}", staged_link.display(), link.display()))?;
        fsync_dir(&self.root)
    }

    pub(super) fn write_record(&self, generation: u64, record: &Record) -> anyhow::Result<()> {
        let path = self.record_path(generation);
        let json = serde_json::to_vec_pretty(record).context("serialise activation record")?;
        mica_fs::Replace::new(&path, "tmp")?
            .mode(FILE_MODE)
            .write(&json)
            .with_context(|| format!("write {}", path.display()))
    }

    pub(super) fn read_record(&self, generation: u64) -> anyhow::Result<Option<Record>> {
        let path = self.record_path(generation);
        match fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(anyhow::Error::new(err)).with_context(|| format!("read {}", path.display()))
            }
        }
    }
}
