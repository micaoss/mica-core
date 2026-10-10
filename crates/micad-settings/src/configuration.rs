//! `/mica/config/` and the baked layer it overrides: the documents, the
//! readers, and the one resolution both callers share.
//!
//! 1. **The baked manifest**, `/usr/share/mica/meta/updates/manifest.json`,
//!    inside the read-only dm-verity root, carries defaults for the source URL,
//!    policy and check interval. Nothing on the device writes it.
//!    Metadata trust keys belong exclusively to the authenticated kernel package.
//! 2. **`/mica/config/updates.json`**, on DATA: operator-owned, and the only
//!    place any of those three is overridden. It also *owns* the keys layer 1
//!    never carries — the windows, the network mode, the workspace paths and
//!    the reboot-gate keys. The independent `/mica/config/fleet.json` document
//!    carries only the fleet overlay.
//! 3. **The running state**, which configures nothing.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod effective;
mod fleet;
mod status;
mod updates;
mod updates_file;
mod window;
pub use effective::*;
pub use fleet::*;
pub use status::*;
pub use updates::*;
pub use updates_file::*;
pub use window::*;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a `/mica/config/` document could not be turned into configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists and could not be read. A missing file is not this: it
    /// is the absent case, and the absent case is the baked defaults.
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bytes are not the document.
    #[error("parse {path}: {message}")]
    Parse { path: PathBuf, message: String },
    /// The document names a trust anchor. Its own variant rather than a
    /// parse error, because it is the one refusal that must survive somebody
    /// widening the schema.
    #[error(
        "{path}: `{key}` names a trust anchor, and anchors are baked into the image. \
         The address this device dials is yours to set; what it will accept is not"
    )]
    Anchor { path: PathBuf, key: String },
    /// The document parses and says something the schema cannot mean.
    #[error("{path}: {message}")]
    Validation { path: PathBuf, message: String },
    /// The document validated and could not be put on the disk. Its own
    /// variant because it is the only one that is not about the operator's
    /// input: the request was right and the device failed it.
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// Layer 1: the baked manifest
// ---------------------------------------------------------------------------

/// Where the build bakes the manifest. `/usr/share/mica/` rather
/// than `/etc/`, because nothing on the device may edit it and `/etc` is where
/// an operator reasonably expects an edit to take.
pub const DEFAULT_MANIFEST_PATH: &str = "/usr/share/mica/meta/updates/manifest.json";

/// The baked document's first key, and the value of it this reader accepts.
pub const MANIFEST_SCHEMA_TAG: &str = "mica/meta/v1";

/// What the device does on its own, and the one key that says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateMode {
    /// The device initiates nothing and no timer arms. Manual check, fetch
    /// and install stay available behind their existing gates, and so does
    /// the offline import: `off` is not "updates disabled", it is "the
    /// device starts nothing".
    Off,
    /// Metadata checks on `checkIntervalMinutes` and nothing else — never
    /// fetches, never installs. The code default, for a device with neither
    /// layer configured.
    #[default]
    Check,
    /// Checks, then fetches, then installs inside a maintenance window, then
    /// reboots or does not per [`RebootPolicy`]. The driver and every gate
    /// it meets are micad's `update_auto`.
    Auto,
}

impl UpdateMode {
    /// The document's spelling, for the recorded state and for a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Check => "check",
            Self::Auto => "auto",
        }
    }
}

/// What the automatic path does once a bundle is installed and the new slot
/// waits for its first boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RebootPolicy {
    /// Stop at `reboot-required` and wait for an operator. The default,
    /// because it is what makes `auto` safe to recommend to someone who has
    /// not read the design.
    #[default]
    Manual,
    /// Reboot inside the same maintenance window, honouring the
    /// safe-to-reboot gate exactly as `Reboot` does — and never arming its
    /// override.
    Window,
}

impl RebootPolicy {
    /// The document's spelling, for the recorded state and for a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Window => "window",
        }
    }
}

/// The baked document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedManifest {
    pub schema: String,
    pub product: Product,
    pub update: BakedUpdate,
    pub http: BakedHttp,
    pub fleet: BakedFleet,
}

/// Which product an image is. A label: nothing reads it to make a decision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Product {
    pub vendor: String,
    pub model: String,
}

/// The update defaults.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedUpdate {
    /// Base URL of the published repository. `null` is a value, not an
    /// omission: no default server.
    pub source: Option<String>,
    pub policy: UpdateMode,
    pub check_interval_minutes: u64,
    /// The core channel this product follows. Absent in a manifest baked
    /// before core channels, which is the general channel.
    #[serde(default = "general_channel")]
    pub core_channel: String,
}

/// The channel every product follows unless it says otherwise.
pub const GENERAL_CHANNEL: &str = "general";

fn general_channel() -> String {
    GENERAL_CHANNEL.to_owned()
}

/// Whether `value` names a core channel: lowercase letters, digits and
/// hyphens, starting with a letter or a digit, at most 64 bytes. The rule
/// `mica/core-set/v1` holds a set's `channel` to.
#[must_use]
pub fn is_core_channel(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedHttp {
    /// Hosts that may receive the configured update credentials in addition
    /// to the baked source's own origin. Empty by default, and adding to it
    /// is a build-time act.
    pub credential_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedFleet {
    pub enabled: bool,
    pub url: Option<String>,
}

impl BakedUpdate {
    /// The defaults a device follows when no manifest could be read.
    ///
    /// No source, so nothing can be checked or fetched against a server this
    /// device was never told about; the cadence is the value the code has
    /// always shipped.
    pub fn code_defaults() -> Self {
        Self {
            source: None,
            policy: UpdateMode::Check,
            check_interval_minutes: 1440,
            core_channel: general_channel(),
        }
    }
}

impl BakedManifest {
    /// The document the reader answers with when there is none to read.
    ///
    /// Empty of everything a device could act on: no source, no trusted key,
    /// no credential host, fleet off. It is a shape, not this device's
    /// configuration, and [`LoadedManifest::error`] is what says so.
    pub fn code_defaults() -> Self {
        Self {
            schema: MANIFEST_SCHEMA_TAG.to_string(),
            product: Product {
                vendor: String::new(),
                model: String::new(),
            },
            update: BakedUpdate::code_defaults(),
            http: BakedHttp {
                credential_hosts: Vec::new(),
            },
            fleet: BakedFleet {
                enabled: false,
                url: None,
            },
        }
    }
}

/// One load of the baked manifest: the document, and the reason it is not
/// this device's when it is not. Both, never neither.
pub struct LoadedManifest {
    pub manifest: BakedManifest,
    /// `Some` when the file is missing, unreadable, or does not parse or
    /// validate. Unreachable on a device by construction — the build refuses
    /// a manifest this reader would reject and the file is inside the verity
    /// root — so it is a reported condition rather than a handled one.
    pub error: Option<String>,
    path: PathBuf,
}

impl LoadedManifest {
    /// The live-state entry: the whole document, the file it came from, and
    /// the error when there is one.
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.path.display().to_string(),
            "error": self.error,
            "document": self.manifest,
        })
    }
}

/// Read the baked manifest at `path`.
pub fn load_manifest(path: &Path) -> LoadedManifest {
    let fallback = |error: String| LoadedManifest {
        manifest: BakedManifest::code_defaults(),
        error: Some(error),
        path: path.to_path_buf(),
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => return fallback(format!("read {}: {err}", path.display())),
    };
    let manifest = match serde_json::from_str::<BakedManifest>(&raw) {
        Ok(manifest) => manifest,
        Err(err) => return fallback(format!("parse {}: {err}", path.display())),
    };
    if manifest.schema != MANIFEST_SCHEMA_TAG {
        return fallback(format!(
            "{}: schema is `{}`, and this reader knows `{MANIFEST_SCHEMA_TAG}`",
            path.display(),
            manifest.schema
        ));
    }
    LoadedManifest {
        manifest,
        error: None,
        path: path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Layer 2: /mica/config/updates.json
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
