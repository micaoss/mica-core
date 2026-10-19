//! The `/mica/ui` bundle store: layout, validation, atomic activation,
//! deactivate, explicit delete, and the "what is installed right now?" read.
//!
//! Layout, all of it under one root (`/mica/ui` on a device, a temporary
//! directory in tests):
//!
//! ```text
//! <root>/bundles/<generation>/   installed trees
//! <root>/current                 symlink to the active bundles/<generation>
//! <root>/records/<generation>.json   digest and compatibility result
//! <root>/.staging-<generation>/  a tree waiting to be validated
//! <root>/.trash-<generation>/    a tree being deleted
//! ```

use anyhow::Context;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

mod install;
mod tree;
mod types;
use tree::*;
pub use types::*;

/// The shipped location of the bundle store.
pub const DEFAULT_ROOT: &str = "/mica/ui";

/// The answer the status read gives when no custom bundle is active. This state
/// is a named answer, never an empty field.
pub const NO_CUSTOM_BUNDLE: &str = "no custom bundle is active; the built-in UI is being served";

/// The optional manifest at the root of a bundle tree.
const MANIFEST_NAME: &str = "mica-ui.json";
/// The one file every bundle must have at its root.
const INDEX_NAME: &str = "index.html";
/// Mode for the root and every directory beneath it.
const DIR_MODE: u32 = 0o755;
/// Mode for every file in the store.
const FILE_MODE: u32 = 0o644;

/// The bundle store rooted at a directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// A store rooted anywhere. Tests root it in a temporary directory; the
    /// device uses [`Store::at_default`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store at `/mica/ui`.
    #[must_use]
    pub fn at_default() -> Self {
        Self::new(DEFAULT_ROOT)
    }

    /// The root this store manages.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an operator places a tree before calling [`Store::activate`].
    #[must_use]
    pub fn staging_dir(&self, generation: u64) -> PathBuf {
        self.root.join(format!(".staging-{generation}"))
    }

    /// The installed tree for a generation.
    #[must_use]
    pub fn bundle_dir(&self, generation: u64) -> PathBuf {
        self.bundles_dir().join(generation.to_string())
    }

    fn bundles_dir(&self) -> PathBuf {
        self.root.join("bundles")
    }

    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }

    fn record_path(&self, generation: u64) -> PathBuf {
        self.records_dir().join(format!("{generation}.json"))
    }

    fn trash_dir(&self, generation: u64) -> PathBuf {
        self.root.join(format!(".trash-{generation}"))
    }

    /// The active pointer.
    #[must_use]
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// The generation `current` points at, or `None` when no bundle is active.
    ///
    /// Reads the link rather than following it, so a `current` left dangling
    /// by a hand-deleted tree still names its generation.
    pub fn active_generation(&self) -> anyhow::Result<Option<u64>> {
        let link = self.current_link();
        let target = match fs::read_link(&link) {
            Ok(target) => target,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(anyhow::Error::new(err))
                    .with_context(|| format!("read link {}", link.display()));
            }
        };
        Ok(target
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<u64>().ok()))
    }

    /// Generations with an installed tree, ascending.
    pub fn generations(&self) -> anyhow::Result<Vec<u64>> {
        numbered_children(&self.bundles_dir())
    }

    /// Every retained generation, newest first, revalidated for display.
    pub fn candidates(&self, served: &[&str]) -> anyhow::Result<Vec<CustomCandidate>> {
        self.generations()?
            .into_iter()
            .rev()
            .map(|generation| self.inspect_generation(generation, served))
            .collect()
    }

    /// The next monotonically increasing generation under the selection lock.
    pub fn next_generation(&self) -> anyhow::Result<u64> {
        self.generations()?
            .into_iter()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("UI generation overflow")
    }

    /// Inspect retained generations and return the newest usable candidate.
    ///
    /// If no generation is usable, the newest installed generation is still
    /// returned with a named reason so the recovery UI can explain why its
    /// custom choice is disabled. A missing store is `Ok(None)` and this read
    /// never creates or repairs anything.
    pub fn available_custom(&self, served: &[&str]) -> anyhow::Result<Option<CustomCandidate>> {
        let mut newest_unusable = None;
        for generation in self.generations()?.into_iter().rev() {
            let candidate = self.inspect_generation(generation, served)?;
            if candidate.usable {
                return Ok(Some(candidate));
            }
            if newest_unusable.is_none() {
                newest_unusable = Some(candidate);
            }
        }
        Ok(newest_unusable)
    }

    /// Revalidate and select the newest usable retained generation.
    ///
    /// The server chooses the generation; callers cannot provide a path or a
    /// generation number. `Ok(None)` means installed trees are absent or all
    /// failed a safety check and leaves `current` untouched.
    #[cfg(test)]
    pub fn select_available_custom(&self, served: &[&str]) -> anyhow::Result<Option<Selection>> {
        let Some(candidate) = self.available_custom(served)? else {
            return Ok(None);
        };
        if !candidate.usable {
            return Ok(None);
        }
        let changed = !self.current_points_at(candidate.generation)?;
        if changed {
            self.point_current_at(candidate.generation)?;
        }
        Ok(Some(Selection { candidate, changed }))
    }

    /// Revalidate and select one explicit retained generation.
    pub fn select_generation(
        &self,
        generation: u64,
        served: &[&str],
    ) -> anyhow::Result<Option<Selection>> {
        if !self.generations()?.contains(&generation) {
            return Ok(None);
        }
        let candidate = self.inspect_generation(generation, served)?;
        if !candidate.usable {
            return Ok(None);
        }
        let changed = !self.current_points_at(generation)?;
        if changed {
            self.point_current_at(generation)?;
        }
        Ok(Some(Selection { candidate, changed }))
    }

    fn current_points_at(&self, generation: u64) -> anyhow::Result<bool> {
        let link = self.current_link();
        match fs::read_link(&link) {
            Ok(target) => {
                let expected = format!("bundles/{generation}");
                Ok(target.as_os_str() == std::ffi::OsStr::new(&expected))
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(false)
            }
            Err(err) => Err(anyhow::Error::new(err))
                .with_context(|| format!("read link {}", link.display())),
        }
    }

    fn inspect_generation(
        &self,
        generation: u64,
        served: &[&str],
    ) -> anyhow::Result<CustomCandidate> {
        let dir = self.bundle_dir(generation);
        let index = dir.join(INDEX_NAME);
        let index_readable = fs::symlink_metadata(&index).is_ok_and(|meta| meta.is_file())
            && File::open(&index).is_ok();
        let unavailable = |reason| CustomCandidate {
            generation,
            manifest: None,
            index_readable,
            digest_matches: None,
            compatible: None,
            digest: None,
            compressed_bytes: None,
            expanded_bytes: None,
            usable: false,
            unavailable_reason: Some(reason),
        };

        let entries = match validate_tree(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                let reason = match err.downcast_ref::<Rejection>() {
                    Some(
                        Rejection::MissingIndex
                        | Rejection::IndexNotRegularFile
                        | Rejection::IndexUnreadable(_),
                    ) => CandidateUnavailable::IndexUnavailable,
                    _ => CandidateUnavailable::UnsafeTree,
                };
                return Ok(unavailable(reason));
            }
        };
        let manifest = match read_manifest(&dir) {
            Ok(manifest) => manifest,
            Err(_) => return Ok(unavailable(CandidateUnavailable::ManifestInvalid)),
        };
        let manifest_summary = manifest.as_ref().map(|manifest| ManifestSummary {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
        });
        let Some(record) = self.read_record(generation)? else {
            let mut candidate = unavailable(CandidateUnavailable::MissingActivationRecord);
            candidate.manifest = manifest_summary;
            return Ok(candidate);
        };
        let digest_matches =
            digest_tree(&dir, &entries).is_ok_and(|digest| digest == record.digest);
        if !digest_matches {
            return Ok(CustomCandidate {
                generation,
                manifest: manifest_summary,
                index_readable,
                digest_matches: Some(false),
                compatible: None,
                digest: Some(record.digest),
                compressed_bytes: record.compressed_bytes,
                expanded_bytes: record.expanded_bytes,
                usable: false,
                unavailable_reason: Some(CandidateUnavailable::DigestMismatch),
            });
        }
        let compatible = match check_compat(manifest.as_ref(), served) {
            Ok(CompatCheck::NotRun) => None,
            Ok(CompatCheck::Ran { compatible, .. }) => Some(compatible),
            Err(err)
                if matches!(
                    err.downcast_ref::<Rejection>(),
                    Some(Rejection::Incompatible { .. })
                ) =>
            {
                return Ok(CustomCandidate {
                    generation,
                    manifest: manifest_summary,
                    index_readable,
                    digest_matches: Some(true),
                    compatible: Some(false),
                    digest: Some(record.digest),
                    compressed_bytes: record.compressed_bytes,
                    expanded_bytes: record.expanded_bytes,
                    usable: false,
                    unavailable_reason: Some(CandidateUnavailable::Incompatible),
                });
            }
            Err(err) => return Err(err),
        };
        Ok(CustomCandidate {
            generation,
            manifest: manifest_summary,
            index_readable,
            digest_matches: Some(true),
            compatible,
            digest: Some(record.digest),
            compressed_bytes: record.compressed_bytes,
            expanded_bytes: record.expanded_bytes,
            usable: true,
            unavailable_reason: None,
        })
    }
}

#[cfg(test)]
mod tests;
