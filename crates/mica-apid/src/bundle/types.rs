//! What the bundle store records, reports and refuses.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

use super::*;

/// `mica-ui.json`. A bundle without one is valid — see [`CompatCheck::NotRun`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    /// Human-readable bundle name.
    pub name: String,
    /// Bundle version, opaque to apid.
    pub version: String,
    /// Directory, relative to the bundle root, whose contents may be served
    /// `Cache-Control: immutable`. Serving it is the asset router's work, not this
    /// module's; the store only carries the declaration.
    #[serde(rename = "immutableDir")]
    pub immutable_dir: String,
    /// The API versions this bundle was built against. Compared with the
    /// served set by **intersection**, never by equality with `current`.
    #[serde(rename = "apiVersions")]
    pub api_versions: Vec<String>,
}

/// Whether class 5's compatibility check ran, and what it decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CompatCheck {
    /// The bundle carries no manifest, so nothing could be checked. Such a
    /// bundle is activated and recorded as unchecked.
    NotRun,
    /// The check ran against a served set.
    Ran {
        /// The manifest's declared API versions.
        declared: Vec<String>,
        /// The served set the declaration was compared against.
        served: Vec<String>,
        /// True when the intersection is non-empty.
        compatible: bool,
    },
}

/// What activation recorded about a generation, on disk under `records/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    /// Digest of the tree as it was activated.
    pub(super) digest: String,
    /// The compatibility check as it stood at activation.
    pub(super) compat: CompatCheck,
    /// Whole uploaded ZIP size; absent for manual installs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compressed_bytes: Option<u64>,
    /// Expanded package size; absent for manual installs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expanded_bytes: Option<u64>,
}

/// The outcome of a successful activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// The generation now pointed at by `current`.
    pub generation: u64,
    /// The digest recorded for it.
    pub digest: String,
    /// The compatibility check recorded for it.
    pub compat: CompatCheck,
    /// The manifest, when the bundle carries one.
    pub manifest: Option<Manifest>,
}

/// A refusal with a reason. Every variant leaves the active bundle untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// `.staging-<generation>` is missing or is not a directory.
    StagingNotDirectory(PathBuf),
    /// `bundles/<generation>` already exists; activation never overwrites one.
    GenerationExists(u64),
    /// No `index.html` at the root of the tree.
    MissingIndex,
    /// `index.html` exists but is not a regular file.
    IndexNotRegularFile,
    /// `index.html` is a regular file that cannot be opened.
    IndexUnreadable(String),
    /// An entry that is neither a regular file nor a directory (rule 2,
    /// rule 5's primary mitigation).
    IrregularEntry {
        /// Path relative to the bundle root.
        path: PathBuf,
        /// What it was instead.
        kind: EntryKind,
    },
    /// A regular file with more than one link — a hardlink, which rule 2
    /// rejects for the same reason as a symlink: it names bytes outside the
    /// tree the operator staged.
    Hardlink {
        /// Path relative to the bundle root.
        path: PathBuf,
        /// The link count found.
        links: u64,
    },
    /// `mica-ui.json` is present but does not parse.
    ManifestUnparsable(String),
    /// The declared range and the served set have no member in common — the
    /// one and only trigger in class 5.
    Incompatible {
        /// The manifest's declared API versions.
        declared: Vec<String>,
        /// The served set compared against.
        served: Vec<String>,
    },
    /// Delete was called on the generation `current` points at. Deactivate
    /// first: the store never unlinks the tree `current` resolves to.
    DeleteWhileActive(u64),
    /// The archive is byte-for-byte the same tree as an installed package.
    DuplicatePackage(u64),
    /// A name/version pair already identifies different installed bytes.
    NameVersionConflict {
        name: String,
        version: String,
        generation: u64,
    },
    /// Uploads never delete older versions implicitly.
    RetentionLimit(usize),
}

/// What an offending entry turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A symbolic link.
    Symlink,
    /// A FIFO.
    Fifo,
    /// A unix socket.
    Socket,
    /// A block device node.
    BlockDevice,
    /// A character device node.
    CharDevice,
    /// Something the kernel reported that is none of the above.
    Unknown,
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Symlink => "symlink",
            Self::Fifo => "fifo",
            Self::Socket => "socket",
            Self::BlockDevice => "block device",
            Self::CharDevice => "character device",
            Self::Unknown => "not a regular file or directory",
        };
        f.write_str(name)
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StagingNotDirectory(path) => {
                write!(f, "{} is not a staged directory", path.display())
            }
            Self::GenerationExists(generation) => {
                write!(f, "bundle generation {generation} already exists")
            }
            Self::MissingIndex => write!(f, "no {INDEX_NAME} at the root of the bundle"),
            Self::IndexNotRegularFile => write!(f, "{INDEX_NAME} is not a regular file"),
            Self::IndexUnreadable(why) => write!(f, "{INDEX_NAME} is unreadable: {why}"),
            Self::IrregularEntry { path, kind } => {
                write!(
                    f,
                    "{} is a {kind}; a bundle carries only regular files and directories",
                    path.display()
                )
            }
            Self::Hardlink { path, links } => {
                write!(
                    f,
                    "{} is a hardlink ({links} links); a bundle carries only single-linked regular files",
                    path.display()
                )
            }
            Self::ManifestUnparsable(why) => write!(f, "{MANIFEST_NAME} does not parse: {why}"),
            Self::Incompatible { declared, served } => write!(
                f,
                "declared API versions [{}] have no member in common with the served set [{}]",
                declared.join(", "),
                served.join(", ")
            ),
            Self::DeleteWhileActive(generation) => write!(
                f,
                "generation {generation} is the active bundle; deactivate before deleting"
            ),
            Self::DuplicatePackage(generation) => write!(
                f,
                "the same package is already installed as generation {generation}"
            ),
            Self::NameVersionConflict {
                name,
                version,
                generation,
            } => write!(
                f,
                "{name} {version} is already generation {generation} with different content"
            ),
            Self::RetentionLimit(limit) => write!(
                f,
                "the store already retains {limit} UI packages; delete one before uploading"
            ),
        }
    }
}

impl std::error::Error for Rejection {}

/// A bundle name/version pair, for the status read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSummary {
    /// `name` from the manifest.
    pub name: String,
    /// `version` from the manifest.
    pub version: String,
}

/// What activation recorded, re-evaluated against the tree as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedState {
    /// The digest recorded at activation.
    pub digest: String,
    /// Whether the tree still hashes to it. A tree that cannot be hashed at
    /// all — an operator planted a symlink in it over a root shell — counts
    /// as a mismatch, because it is one.
    pub digest_matches: bool,
    /// The compatibility check as recorded at activation.
    pub compat: CompatCheck,
}

/// The active bundle, read from the served tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomUi {
    /// The generation `current` resolves to.
    pub generation: u64,
    /// The manifest read from the tree **now**, or `None` for "no manifest".
    pub manifest: Option<ManifestSummary>,
    /// Whether `index.html` exists and opens **right now**.
    pub index_readable: bool,
    /// What activation recorded, or `None` when this generation has no
    /// activation record — a tree placed under `bundles/` by hand.
    pub recorded: Option<RecordedState>,
}

/// Why an installed custom UI cannot currently be selected.
///
/// These are deliberately stable, path-free states suitable for an API
/// response. Detailed I/O failures stay in apid's logs rather than exposing
/// filesystem layout to a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateUnavailable {
    /// The installed tree has no valid activation record.
    MissingActivationRecord,
    /// The tree contains an irregular entry or could not be walked safely.
    UnsafeTree,
    /// The root `index.html` is missing, irregular or unreadable.
    IndexUnavailable,
    /// The optional manifest is present but cannot be read or parsed.
    ManifestInvalid,
    /// The installed tree no longer matches its activation digest.
    DigestMismatch,
    /// The manifest does not support an API version served by this apid.
    Incompatible,
}

/// One retained generation inspected against the API versions served now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCandidate {
    /// The installed generation.
    pub generation: u64,
    /// Name and version from a valid manifest, when one exists.
    pub manifest: Option<ManifestSummary>,
    /// Whether the root `index.html` is a readable regular file.
    pub index_readable: bool,
    /// Whether the current tree matches the activation record, when one was
    /// available and the tree could be hashed.
    pub digest_matches: Option<bool>,
    /// Whether the manifest intersects the currently served API set. `None`
    /// means the bundle has no manifest and is therefore unchecked.
    pub compatible: Option<bool>,
    /// SHA-256 recorded when the generation was installed.
    pub digest: Option<String>,
    /// Whole uploaded ZIP size, absent for manual generations.
    pub compressed_bytes: Option<u64>,
    /// Expanded package size, absent for manual generations.
    pub expanded_bytes: Option<u64>,
    /// True only when the generation can safely be selected now.
    pub usable: bool,
    /// The first failed safety check, absent when [`Self::usable`] is true.
    pub unavailable_reason: Option<CandidateUnavailable>,
}

/// The result of selecting a retained custom UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The fully revalidated candidate selected by the operation.
    pub candidate: CustomCandidate,
    /// False when `current` already pointed at this generation.
    pub changed: bool,
}

/// The answer to "what is installed right now?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// No custom bundle is active; the built-in UI is being served. This is a
    /// named answer, not an absent one.
    BuiltIn,
    /// A custom bundle is active.
    Custom(CustomUi),
}

/// The result of re-evaluating the active bundle.
///
/// Every field is a state, never an error: a bundle must never be able to
/// fail apid's start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recheck {
    /// The active generation.
    pub generation: u64,
    /// Whether the tree still matches its recorded digest; `None` when no
    /// digest was recorded for this generation.
    pub digest_matches: Option<bool>,
    /// The compatibility check re-run against the served set passed in.
    pub compat: CompatCheck,
}

impl Recheck {
    /// True when the tree no longer matches the digest recorded at activation.
    #[must_use]
    pub fn corrupt(&self) -> bool {
        self.digest_matches == Some(false)
    }
}
