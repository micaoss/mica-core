//! The provisioning DOCUMENT: a versioned, validated, idempotent file that
//! configures a device with no network at all.
//!
//! # The no-network property, held the same way Layer 1 holds it

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use micad_settings::{IfaceSettings, MIN_ADMIN_PASSWORD_LEN, TimeSettings, WifiClientSettings};

mod apply;
mod import;
mod parse;
use apply::*;
pub use import::*;
use parse::*;

/// The document schema version this build applies.
///
/// The DOCUMENT's own version, in its own first field, independent of
/// `micad_settings::SCHEMA_VERSION`: a document format revision does not
/// reshape the settings tree, and a settings bump does not invalidate a
/// document an operator already wrote onto a card.
pub const DOCUMENT_VERSION: u32 = 1;

/// The one file name a document may have, at the root of a source.
pub const DOCUMENT_FILE_NAME: &str = "mica-provisioning.toml";

/// Where the transport unit stages the sources it found, read-only.
///
/// `/run` and not `/mnt`: the staging tree is per-boot, it must never survive
/// into a state anything else reads, and `/run` is the tmpfs systemd
/// guarantees exists before any unit that could mount into it.
pub const DEFAULT_STAGING_ROOT: &str = "/run/mica/provisioning";

/// The largest document this daemon will read.
const MAX_DOCUMENT_BYTES: u64 = 64 * 1024;

/// What a shape failure reports, in place of the parser's own message.
///
/// `toml`'s type errors quote the offending literal (`invalid type: integer
/// 5, expected a string`), and the offending literal may be the administrator
/// password. The key path says where to look; a TOML linter run against the
/// file before it goes on the medium says what is wrong with it.
pub const SHAPE_REASON: &str = "this key does not have the shape the document schema requires (the parser's own message is \
     not repeated here: it would quote the offending value, and a value in this document may be \
     a secret)";

/// Which transport offered a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The BOOT partition, as the transport unit staged it.
    Boot,
    /// An attached removable device, as the transport unit staged it.
    Media,
}

/// The sources consulted, in the order they are consulted.
pub const SOURCES: [Source; 2] = [Source::Boot, Source::Media];

impl Source {
    /// The name this source is recorded and reported under.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Media => "media",
        }
    }

    /// The directory under the staging root this source is staged at.
    ///
    /// The same string as [`Source::as_str`], and deliberately a separate
    /// method: the recorded name is a wire value the status route serves and
    /// the directory name is a contract with the transport unit, and the day
    /// one has to change the other must not follow it silently.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::Media => "media",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a document was refused.
///
/// **A key path and a reason, never a value.** Both halves are load-bearing:
/// the key path is what makes a refusal actionable to whoever wrote the file,
/// and the absence of the value is what makes it safe to log, record and
/// serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// Dotted path of the offending key, or empty when the document as a whole
    /// is the problem (unreadable, not TOML).
    pub key: String,
    /// What is wrong with it.
    pub reason: String,
}

impl Rejection {
    fn at(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            reason: reason.into(),
        }
    }

    fn whole(reason: impl Into<String>) -> Self {
        Self {
            key: String::new(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.key.is_empty() {
            formatter.write_str(&self.reason)
        } else {
            write!(formatter, "`{}`: {}", self.key, self.reason)
        }
    }
}

/// What [`import`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Neither source carried a document. Nothing was read, written or
    /// recorded — a device that boots without a document must not lose the
    /// record of the one it applied last time.
    NoDocument,
    /// A document was applied and committed.
    Applied {
        /// Which source it came from.
        source: Source,
        /// Its `version`.
        version: u32,
        /// Its canonical digest.
        digest: String,
    },
    /// The offered document is the one already applied; nothing was written.
    Unchanged {
        /// Which source offered it.
        source: Source,
        /// Its canonical digest, which is the recorded one.
        digest: String,
    },
    /// The document was refused. Nothing it names was applied; the only thing
    /// written is the import record itself.
    Rejected {
        /// Which source offered it.
        source: Source,
        /// Why.
        rejection: Rejection,
    },
}

/// The refusal a short administrator password earns.
///
/// Its own function so the sentence exists once, and so the rule that it names
/// neither the value nor its length is stated where it is enforced: this
/// string reaches a log, the import record and an HTTP client.
fn too_short_password() -> Rejection {
    Rejection::at(
        "admin.password",
        format!(
            "an administrator bootstrap password is at least {MIN_ADMIN_PASSWORD_LEN} characters"
        ),
    )
}

/// The provisioning document, as parsed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProvisioningDocument {
    /// The document schema version; must equal [`DOCUMENT_VERSION`].
    pub version: u32,
    /// Device identity the factory injects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentitySection>,
    /// Administrator bootstrap credential material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<AdminSection>,
    /// Per-interface network configuration, written to `network`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<BTreeMap<String, IfaceSettings>>,
    /// WiFi station configuration, written to `wifi.client`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wifi: Option<WifiClientSettings>,
    /// NTP servers and the presentation timezone, written to `time`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeSettings>,
}

/// `[identity]`: what the factory injects, and what it must not.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct IdentitySection {
    /// `provisioning.deviceId`: 32 lowercase hex characters.
    #[serde(rename = "deviceId", skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

/// `[admin]`: how the first administrator gets in.
///
/// **Secret-bearing.** `password` is the plaintext the operator will type; it
/// is hashed with Argon2id ([`identity::hash_password`]) into
/// `access.webAdmin.password_hash` and the plaintext is dropped. It is never
/// stored, logged or echoed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AdminSection {
    /// The administrator bootstrap password, plaintext. SECRET.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Authorized-key lines, in `authorized_keys` syntax, written to
    /// `access.ssh.authorizedKeys`. Declarative: the listed set REPLACES the
    /// stored one, which on the unclaimed device this document applies to is
    /// empty.
    #[serde(rename = "authorizedKeys", skip_serializing_if = "Option::is_none")]
    pub authorized_keys: Option<Vec<String>>,
}

/// Top-level keys a document may carry.
const DOCUMENT_KEYS: [&str; 6] = ["version", "identity", "admin", "network", "wifi", "time"];

/// Keys `[identity]` may carry.
const IDENTITY_KEYS: [&str; 1] = ["deviceId"];

/// Keys `[admin]` may carry.
const ADMIN_KEYS: [&str; 2] = ["password", "authorizedKeys"];

/// The canonical digest of a document: SHA-256 over its canonical rendering,
/// lowercase hex.
#[must_use]
pub fn digest_of(document: &ProvisioningDocument) -> String {
    let canonical = toml::to_string(document).unwrap_or_default();
    hex::encode(aws_lc_rs::digest::digest(
        &aws_lc_rs::digest::SHA256,
        canonical.as_bytes(),
    ))
}

/// The staging root to read, honouring the `MICAD_PROVISIONING_ROOT` test hook.
#[must_use]
pub fn staging_root_from_env() -> PathBuf {
    std::env::var_os("MICAD_PROVISIONING_ROOT")
        .map_or_else(|| PathBuf::from(DEFAULT_STAGING_ROOT), PathBuf::from)
}

#[cfg(test)]
mod tests;
