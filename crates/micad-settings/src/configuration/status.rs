//! The provisioning status: what the operator wrote and what is in effect.

use serde_json::{Map, Value, json};
use std::path::Path;

use super::*;

/// `{ "operator": …, "effective": … }` for the six update fields, resolved
/// through the same reader and the same precedence micad runs on.
///
/// **A key the operator did not write is absent; a key they wrote as `null`
/// is `null`.** Both resolve to the baked default, and they are still two
/// different statements about the document.
pub fn provisioning_status_at(
    manifest: &Path,
    updates: &Path,
    fleet: &Path,
) -> Result<Value, ConfigError> {
    let baked = load_manifest(manifest).manifest;
    let document = load_updates(updates)?;
    let fleet_document = load_fleet(fleet)?;

    let mut operator_update = Map::new();
    if let Some(url) = &document.source.url {
        operator_update.insert("source".into(), json!(url));
    }
    if let Some(policy) = &document.policy {
        operator_update.insert("policy".into(), json!(policy));
    }
    if let Some(channel) = &document.core_channel {
        operator_update.insert("coreChannel".into(), json!(channel));
    }
    let mut operator = Map::new();
    if !operator_update.is_empty() {
        operator.insert("update".into(), Value::Object(operator_update));
    }

    if let Some(document) = &fleet_document {
        let mut operator_fleet = Map::new();
        if let Some(enabled) = document.enabled {
            operator_fleet.insert("enabled".into(), json!(enabled));
        }
        if let Some(reporting) = document.reporting {
            operator_fleet.insert("reporting".into(), json!(reporting));
        }
        if let Some(url) = &document.url {
            operator_fleet.insert("url".into(), json!(url));
        }
        if !operator_fleet.is_empty() {
            operator.insert("fleet".into(), Value::Object(operator_fleet));
        }
    }

    let effective = resolve(&baked.update, document);
    let fleet = effective_fleet(&baked.fleet, fleet_document.as_ref());
    // Unreachable: `resolve` always produces one, and the path that does not
    // is `Err` above. Reported rather than unwrapped, because the whole rule
    // is that nothing substitutes a baked value for an unknown one.
    let selection = effective
        .selection
        .as_ref()
        .ok_or_else(|| ConfigError::Validation {
            path: updates.to_path_buf(),
            message: "the document did not resolve to a selection".to_string(),
        })?;

    Ok(json!({
        "operator": Value::Object(operator),
        "effective": {
            "update": {
                "source": selection.url,
                "policy": selection.mode,
                "coreChannel": selection.core_channel,
            },
            "fleet": {
                "url": fleet.url,
                "enabled": fleet.enabled,
                "reporting": fleet.reporting,
            },
        },
    }))
}
