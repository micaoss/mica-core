//! Applying a validated document to the settings.

use crate::identity;
use anyhow::Result;
use micad_settings::Settings;

use super::*;

/// Write a validated document into `settings`.
pub(super) fn apply_into(
    document: &ProvisioningDocument,
    settings: &mut Settings,
) -> Result<(), Rejection> {
    if let Some(identity) = &document.identity
        && let Some(device_id) = &identity.device_id
    {
        set(settings, "provisioning.deviceId", device_id.as_str().into())?;
    }
    if let Some(admin) = &document.admin {
        if let Some(password) = &admin.password {
            // Argon2id, the format `access.webAdmin.password_hash` holds and
            // apid verifies against. The plaintext is dropped here: it is not
            // stored, and there is deliberately no path that could read it
            // back.
            let hash = identity::hash_password(password).map_err(|_| {
                Rejection::at(
                    "admin.password",
                    "the administrator password could not be hashed",
                )
            })?;
            set(
                settings,
                "access.webAdmin",
                serde_json::json!({ "password_hash": hash }),
            )?;
        }
        if let Some(lines) = &admin.authorized_keys {
            let keys = parse_keys(lines)?;
            let value = serde_json::to_value(&keys).map_err(|_| {
                Rejection::at("admin.authorizedKeys", "the key list could not be encoded")
            })?;
            set(settings, "access.ssh.authorizedKeys", value)?;
        }
    }
    if let Some(network) = &document.network {
        set(settings, "network", encode(network, "network")?)?;
    }
    if let Some(wifi) = &document.wifi {
        set(settings, "wifi.client", encode(wifi, "wifi")?)?;
    }
    if let Some(time) = &document.time {
        set(settings, "time", encode(time, "time")?)?;
    }
    Ok(())
}

/// One dot-path write, refusing with the settings tree's own sentence.
pub(super) fn set(
    settings: &mut Settings,
    path: &str,
    value: serde_json::Value,
) -> Result<(), Rejection> {
    settings.set(path, value).map_err(|err| {
        Rejection::at(
            document_key_for(path),
            match err {
                micad_settings::SettingsError::Validation { message, .. } => message,
                other => other.to_string(),
            },
        )
    })
}

/// The DOCUMENT key a settings dot-path came from.
///
/// A refusal has to name a key the person holding the file can find, and the
/// settings path and the document path are not the same string for three of
/// the six sections.
pub(super) fn document_key_for(path: &str) -> &str {
    match path {
        "provisioning.deviceId" => "identity.deviceId",
        "access.webAdmin" => "admin.password",
        "access.ssh.authorizedKeys" => "admin.authorizedKeys",
        "wifi.client" => "wifi",
        other => other,
    }
}

/// A section as JSON, for [`Settings::set`].
pub(super) fn encode<T: serde::Serialize>(
    value: &T,
    key: &str,
) -> Result<serde_json::Value, Rejection> {
    serde_json::to_value(value).map_err(|_| Rejection::at(key, "this section could not be encoded"))
}
