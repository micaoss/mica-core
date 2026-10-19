//! Reading and validating a provisioning document.

use anyhow::Result;
use micad_settings::{
    AuthorizedKey, MIN_ADMIN_PASSWORD_LEN, is_wpa_quotable, parse_authorized_key,
    validate_authorized_keys, validate_device_id, validate_ntp_servers, validate_timezone_name,
    validate_wifi_psk,
};
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// The first source under `staging_root` carrying a document.
pub(super) fn find_document(staging_root: &Path) -> Option<(Source, PathBuf)> {
    SOURCES.into_iter().find_map(|source| {
        let path = staging_root
            .join(source.dir_name())
            .join(DOCUMENT_FILE_NAME);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() {
            tracing::warn!(
                %source,
                path = %path.display(),
                "provisioning document path is not a regular file; ignored"
            );
            return None;
        }
        Some((source, path))
    })
}

/// Read and parse the document at `path`.
pub(super) fn read_document(path: &Path) -> Result<ProvisioningDocument, Rejection> {
    let size = fs::symlink_metadata(path)
        .map_err(|_| Rejection::whole("the document could not be read"))?
        .len();
    if size > MAX_DOCUMENT_BYTES {
        return Err(Rejection::whole(format!(
            "the document is larger than the {MAX_DOCUMENT_BYTES}-byte maximum"
        )));
    }
    let text = fs::read_to_string(path)
        .map_err(|_| Rejection::whole("the document could not be read as UTF-8 text"))?;
    let table: toml::Table = text
        .parse()
        .map_err(|_: toml::de::Error| Rejection::whole("the document is not valid TOML"))?;
    parse_document(&table)
}

/// Turn a parsed TOML table into a document, key by key.
///
/// By hand rather than by `Deserialize`, for the reason [`SHAPE_REASON`]
/// gives: a derived deserializer reports a type error by quoting the offending
/// literal, and this document's literals include the administrator password.
/// Extracting key by key means every message in this file is one written here.
pub(super) fn parse_document(table: &toml::Table) -> Result<ProvisioningDocument, Rejection> {
    reject_unknown_keys(table, &DOCUMENT_KEYS, "")?;

    let version = match table.get("version") {
        None => {
            return Err(Rejection::at(
                "version",
                "the document must state its schema version in its first field",
            ));
        }
        Some(toml::Value::Integer(version)) => u32::try_from(*version)
            .map_err(|_| Rejection::at("version", "the document version is out of range"))?,
        Some(_) => return Err(Rejection::at("version", SHAPE_REASON)),
    };

    let identity = match table.get("identity") {
        None => None,
        Some(toml::Value::Table(section)) => {
            reject_unknown_keys(section, &IDENTITY_KEYS, "identity")?;
            Some(IdentitySection {
                device_id: optional_string(section, "deviceId", "identity.deviceId")?,
            })
        }
        Some(_) => return Err(Rejection::at("identity", SHAPE_REASON)),
    };

    let admin = match table.get("admin") {
        None => None,
        Some(toml::Value::Table(section)) => {
            reject_unknown_keys(section, &ADMIN_KEYS, "admin")?;
            Some(AdminSection {
                password: optional_string(section, "password", "admin.password")?,
                authorized_keys: optional_string_list(
                    section,
                    "authorizedKeys",
                    "admin.authorizedKeys",
                )?,
            })
        }
        Some(_) => return Err(Rejection::at("admin", SHAPE_REASON)),
    };

    Ok(ProvisioningDocument {
        version,
        identity,
        admin,
        network: typed_section(table, "network")?,
        wifi: typed_section(table, "wifi")?,
        time: typed_section(table, "time")?,
    })
}

/// Refuse any key of `table` that is not in `allowed`, naming it.
///
/// The precise half of the shape rule, and the one an operator hits: a
/// `[certificates]` section, or `deviceID` for `deviceId`, is named exactly
/// rather than reported as a shape failure of the whole document.
pub(super) fn reject_unknown_keys(
    table: &toml::Table,
    allowed: &[&str],
    prefix: &str,
) -> Result<(), Rejection> {
    for key in table.keys() {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        return Err(Rejection::at(
            path,
            format!(
                "the document schema has no such key; it may carry {}",
                allowed.join(", ")
            ),
        ));
    }
    Ok(())
}

/// An optional string field.
pub(super) fn optional_string(
    table: &toml::Table,
    key: &str,
    path: &str,
) -> Result<Option<String>, Rejection> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(Rejection::at(path, SHAPE_REASON)),
    }
}

/// An optional array-of-strings field.
pub(super) fn optional_string_list(
    table: &toml::Table,
    key: &str,
    path: &str,
) -> Result<Option<Vec<String>>, Rejection> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                toml::Value::String(value) => Ok(value.clone()),
                _ => Err(Rejection::at(path, SHAPE_REASON)),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(Rejection::at(path, SHAPE_REASON)),
    }
}

/// A section deserialized straight into the settings type that owns it.
///
/// The serde message is dropped for [`SHAPE_REASON`]'s reason. What is lost is
/// precision inside these three sections; what is kept is the guarantee that
/// no message this module produces can quote a value.
pub(super) fn typed_section<T: serde::de::DeserializeOwned>(
    table: &toml::Table,
    key: &str,
) -> Result<Option<T>, Rejection> {
    match table.get(key) {
        None => Ok(None),
        Some(value) => value
            .clone()
            .try_into()
            .map(Some)
            .map_err(|_: toml::de::Error| Rejection::at(key, SHAPE_REASON)),
    }
}

/// Validate the WHOLE document. Nothing is applied until this has passed.
///
/// Every predicate is one `micad_settings` already states, so a document can
/// express exactly what the settings tree accepts and no more — there is no
/// second grammar here that could drift from the first.
pub(super) fn validate(document: &ProvisioningDocument) -> Result<(), Rejection> {
    if document.version != DOCUMENT_VERSION {
        return Err(Rejection::at(
            "version",
            format!("this build applies provisioning document version {DOCUMENT_VERSION}"),
        ));
    }
    if let Some(identity) = &document.identity
        && let Some(device_id) = &identity.device_id
    {
        validate_device_id(device_id)
            .map_err(|reason| Rejection::at("identity.deviceId", reason))?;
    }
    if let Some(admin) = &document.admin {
        if let Some(password) = &admin.password
            && password.len() < MIN_ADMIN_PASSWORD_LEN
        {
            return Err(too_short_password());
        }
        if let Some(lines) = &admin.authorized_keys {
            validate_authorized_keys(&parse_keys(lines)?)
                .map_err(|err| Rejection::at("admin.authorizedKeys", refusal_sentence(&err)))?;
        }
    }
    if let Some(wifi) = &document.wifi {
        for (index, network) in wifi.networks.iter().enumerate() {
            let path = format!("wifi.networks[{index}]");
            if network.ssid.is_empty() || !is_wpa_quotable(&network.ssid) {
                return Err(Rejection::at(
                    format!("{path}.ssid"),
                    "a network name must be non-empty printable ASCII without a quote or a \
                     backslash, which is what wpa_supplicant configuration can carry",
                ));
            }
            if let Some(psk) = &network.psk {
                validate_wifi_psk(psk)
                    .map_err(|reason| Rejection::at(format!("{path}.psk"), reason))?;
            }
        }
    }
    if let Some(time) = &document.time {
        validate_ntp_servers(&time.ntp.servers)
            .map_err(|reason| Rejection::at("time.ntp.servers", reason))?;
        validate_timezone_name(&time.timezone)
            .map_err(|reason| Rejection::at("time.timezone", reason))?;
    }
    Ok(())
}

/// Parse every authorized-key line, or refuse naming the entry.
pub(super) fn parse_keys(lines: &[String]) -> Result<Vec<AuthorizedKey>, Rejection> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            parse_authorized_key(line).map_err(|err| {
                Rejection::at(
                    format!("admin.authorizedKeys[{index}]"),
                    refusal_sentence(&err),
                )
            })
        })
        .collect()
}

/// The sentence a `SettingsError` refusal carries.
///
/// `parse_authorized_key` and `validate_authorized_keys` both document that
/// they never echo the rejected input, so their message is safe to carry.
pub(super) fn refusal_sentence(err: &micad_settings::SettingsError) -> String {
    match err {
        micad_settings::SettingsError::Validation { message, .. } => message.clone(),
        other => other.to_string(),
    }
}
