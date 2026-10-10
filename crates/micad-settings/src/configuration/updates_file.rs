//! Reading, validating, patching and writing the update document.

use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

use super::*;

/// Read the operator document at `path`.
pub fn load_updates(path: &Path) -> Result<UpdatesDocument, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UpdatesDocument::default());
        }
        Err(err) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    // Parsed to a `Value` first so the anchor scan sees every key the
    // document names, including ones a widened schema would accept.
    let value = serde_json::from_str::<Value>(&raw).map_err(|err| ConfigError::Parse {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    if let Some(key) = anchor_key(&value) {
        return Err(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        });
    }
    let document =
        serde_json::from_value::<UpdatesDocument>(value).map_err(|err| ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    validate(&document).map_err(|message| ConfigError::Validation {
        path: path.to_path_buf(),
        message,
    })?;
    Ok(document)
}

/// The first anchor-shaped key `value` names, at any depth, or `None`.
///
/// Arrays are walked as well as objects: a `trust` block inside a maintenance
/// window would be refused by `deny_unknown_fields` anyway, but this scan is
/// the one that must not have a hole in it.
pub(super) fn anchor_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if ANCHOR_KEYS
                    .iter()
                    .any(|anchor| anchor.eq_ignore_ascii_case(key))
                {
                    return Some(key.clone());
                }
                if let Some(found) = anchor_key(child) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(anchor_key),
        _ => None,
    }
}

/// Validate what serde cannot: the schema tag when the document names one,
/// that window times parse and days are day names, and that a document
/// *naming* `auto` names a window to install in.
pub fn validate(document: &UpdatesDocument) -> Result<(), String> {
    if let Some(schema) = &document.schema
        && schema != UPDATES_SCHEMA_TAG
    {
        return Err(format!(
            "schema is `{schema}`, and this reader knows `{UPDATES_SCHEMA_TAG}`"
        ));
    }
    for window in &document.maintenance.windows {
        minutes_of_day(&window.start)
            .ok_or_else(|| format!("maintenance window start `{}` is not HH:MM", window.start))?;
        minutes_of_day(&window.end)
            .ok_or_else(|| format!("maintenance window end `{}` is not HH:MM", window.end))?;
        for day in &window.days {
            day_index(day).ok_or_else(|| {
                format!("maintenance window day `{day}` is not mon/tue/wed/thu/fri/sat/sun")
            })?;
        }
    }
    if let Some(check_at) = document.check_at.as_ref().and_then(Option::as_ref) {
        minutes_of_day(check_at).ok_or_else(|| format!("checkAt `{check_at}` is not HH:MM"))?;
    }
    if let Some(channel) = document.core_channel.as_ref().and_then(Option::as_ref)
        && !crate::configuration::is_core_channel(channel)
    {
        return Err(format!(
            "coreChannel `{channel}` is not a channel name: lowercase letters, digits and hyphens"
        ));
    }
    // The document-local half of the rule. The half precedence creates -- a
    // baked `auto` under a document that names no policy -- cannot be seen
    // from here, and is [`EffectivePolicy::auto_window_refusal`].
    if document.policy.flatten() == Some(UpdateMode::Auto)
        && document.maintenance.windows.is_empty()
    {
        return Err(AUTO_NEEDS_A_WINDOW.to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

/// A change to the operator document: the keys the write route names, merged
/// over what is on the disk.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesPatch {
    /// What the device does on its own.
    #[serde(default, deserialize_with = "present")]
    pub policy: Override<UpdateMode>,
    /// Minutes between automatic checks; `0` disables them.
    #[serde(default, deserialize_with = "present")]
    pub check_interval_minutes: Override<u64>,
    /// `HH:MM` UTC to anchor the check to; `null` returns it to the interval.
    #[serde(default, deserialize_with = "present")]
    pub check_at: Override<String>,
    /// The core channel to follow; `null` returns it to the baked channel.
    #[serde(default, deserialize_with = "present")]
    pub core_channel: Override<String>,
    /// What the automatic path does after an install. Not an override —
    /// layer 1 bakes no default for it — so it has two states, not three.
    #[serde(default)]
    pub reboot_policy: Option<RebootPolicy>,
    #[serde(default)]
    pub source: Option<UpdatesSourcePatch>,
    /// Replaced whole when named: the object's own serde defaults apply to
    /// the keys the caller leaves out of it.
    #[serde(default)]
    pub network: Option<NetworkPolicy>,
    #[serde(default)]
    pub maintenance: Option<MaintenancePolicy>,
    #[serde(default)]
    pub reboot_gate: Option<RebootGatePolicy>,
}

/// The overridable key of `source`, and nothing else it holds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesSourcePatch {
    /// Where this device dials. `null` returns it to the baked address —
    /// which is a *default*, never a fallback.
    #[serde(default, deserialize_with = "present")]
    pub url: Override<String>,
}

/// Merge `patch` over `document`, key by key.
pub fn apply_patch(mut document: UpdatesDocument, patch: UpdatesPatch) -> UpdatesDocument {
    if let Some(policy) = patch.policy {
        document.policy = Some(policy);
    }
    if let Some(minutes) = patch.check_interval_minutes {
        document.check_interval_minutes = Some(minutes);
    }
    if let Some(check_at) = patch.check_at {
        document.check_at = Some(check_at);
    }
    if let Some(core_channel) = patch.core_channel {
        document.core_channel = Some(core_channel);
    }
    if let Some(reboot_policy) = patch.reboot_policy {
        document.reboot_policy = reboot_policy;
    }
    if let Some(url) = patch.source.and_then(|source| source.url) {
        document.source.url = Some(url);
    }
    if let Some(network) = patch.network {
        document.network = network;
    }
    if let Some(maintenance) = patch.maintenance {
        document.maintenance = maintenance;
    }
    if let Some(reboot_gate) = patch.reboot_gate {
        document.reboot_gate = reboot_gate;
    }
    document
}

/// Why a write did not happen, split by **whose** problem it is.
#[derive(Debug, thiserror::Error)]
pub enum WriteRefusal {
    /// The patch itself, or the document it would produce: not JSON, an
    /// unknown key, a trust anchor, or a value the reader would refuse.
    #[error(transparent)]
    Rejected(ConfigError),
    /// The document on the disk did not load, so there is no base to merge
    /// over. Nothing was written.
    #[error(transparent)]
    Unreadable(ConfigError),
    /// It validated and the device could not store it.
    #[error(transparent)]
    Unwritable(ConfigError),
}

/// Apply `patch_json` to the document at `path` and write the result.
///
/// Answers the document as saved, which is what the operator's layer now says.
///
/// # Errors
pub fn write_updates(path: &Path, patch_json: &str) -> Result<UpdatesDocument, WriteRefusal> {
    let value = serde_json::from_str::<Value>(patch_json).map_err(|err| {
        WriteRefusal::Rejected(ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    })?;
    // Before deserialization and at any depth, exactly as the reader does it:
    // an anchor must be refused by name rather than by whatever
    // `deny_unknown_fields` happens to say on the day the schema grows.
    // The write route is the surface where somebody would
    // *try*.
    if let Some(key) = anchor_key(&value) {
        return Err(WriteRefusal::Rejected(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        }));
    }
    let patch = serde_json::from_value::<UpdatesPatch>(value).map_err(|err| {
        WriteRefusal::Rejected(ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    })?;
    let base = load_updates(path).map_err(WriteRefusal::Unreadable)?;
    let document = apply_patch(base, patch);
    save_updates(path, &document).map_err(|err| match err {
        validation @ ConfigError::Validation { .. } => WriteRefusal::Rejected(validation),
        other => WriteRefusal::Unwritable(other),
    })?;
    Ok(document)
}

/// Validate `document` and replace the file at `path` with it, atomically.
///
/// **Validation first, and the same [`validate`] the reader runs.** The
/// `auto`-requires-a-window rule fires here, at the API, rather than hours
/// later at the next check — which is the whole reason the rule has two
/// callers.
pub fn save_updates(path: &Path, document: &UpdatesDocument) -> Result<(), ConfigError> {
    validate(document).map_err(|message| ConfigError::Validation {
        path: path.to_path_buf(),
        message,
    })?;
    let mut stamped = document.clone();
    stamped.schema = Some(UPDATES_SCHEMA_TAG.to_string());
    let mut text = serde_json::to_string_pretty(&stamped).map_err(|err| ConfigError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::other(err),
    })?;
    text.push('\n');
    crate::store::write_atomically(path, &text).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

// ---------------------------------------------------------------------------
// The resolution
// ---------------------------------------------------------------------------
