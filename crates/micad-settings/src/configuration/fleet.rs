//! The fleet document, `/mica/config/fleet.json`.

use serde::Deserialize;
use serde::de::Deserializer;
use serde_json::Value;
use std::path::Path;

use super::*;

/// Where the desired fleet configuration is poured on the DATA pool.
pub const DEFAULT_FLEET_PATH: &str = "/mica/config/fleet.json";

/// The only fleet document schema this development tree accepts.
pub(super) const FLEET_SCHEMA_TAG: &str = "mica/fleet-config/v1";

/// The operator's desired fleet overlay, before baked defaults are applied.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FleetDocument {
    pub(super) schema: String,
    #[serde(default, deserialize_with = "present_boolean")]
    pub(super) enabled: Option<bool>,
    #[serde(default, deserialize_with = "present_boolean")]
    pub(super) reporting: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    pub(super) url: Override<String>,
}

/// Keep an omitted boolean optional while rejecting explicit `null`.
pub(super) fn present_boolean<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map(Some)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct EffectiveFleet {
    pub(super) enabled: bool,
    pub(super) reporting: bool,
    pub(super) url: Option<String>,
}

/// Read the current fleet overlay. Absence selects baked defaults; every
/// present document must parse and validate completely.
pub(super) fn load_fleet(path: &Path) -> Result<Option<FleetDocument>, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|_| ConfigError::Parse {
        path: path.to_path_buf(),
        message: "invalid JSON document".to_string(),
    })?;
    if let Some(key) = anchor_key(&value) {
        return Err(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        });
    }
    // Deserialize from the original text, not `value`: serde's struct visitor
    // rejects duplicate fields, while a generic JSON map has already replaced
    // an earlier duplicate by the time it exists.
    let document = serde_json::from_str::<FleetDocument>(&raw).map_err(|_| ConfigError::Parse {
        path: path.to_path_buf(),
        message: "document does not match the fleet configuration schema".to_string(),
    })?;
    if document.schema != FLEET_SCHEMA_TAG {
        return Err(ConfigError::Validation {
            path: path.to_path_buf(),
            message: format!("unsupported schema; expected `{FLEET_SCHEMA_TAG}`"),
        });
    }
    if let Some(Some(value)) = &document.url {
        let valid = url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        });
        if !valid {
            return Err(ConfigError::Validation {
                path: path.to_path_buf(),
                message: "`url` must be an HTTPS URL without userinfo".to_string(),
            });
        }
    }
    Ok(Some(document))
}

pub(super) fn effective_fleet(
    baked: &BakedFleet,
    document: Option<&FleetDocument>,
) -> EffectiveFleet {
    let enabled = document
        .and_then(|document| document.enabled)
        .unwrap_or(baked.enabled);
    EffectiveFleet {
        enabled,
        reporting: enabled
            && document
                .and_then(|document| document.reporting)
                .unwrap_or(true),
        url: document
            .and_then(|document| document.url.clone())
            .flatten()
            .or_else(|| baked.url.clone()),
    }
}

// ---------------------------------------------------------------------------
// the read surface
// ---------------------------------------------------------------------------
