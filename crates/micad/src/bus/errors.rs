//! How settings faults and refusals reach the bus.

use crate::update_lifecycle::Refusal;
use micad_settings::SettingsError;
use zbus::fdo;
use zbus::message::Header;

/// Unique bus name of the caller, or `"(unknown)"` on an unnamed message.
///
/// Used by management methods so audit state names the exact D-Bus caller.
pub(crate) fn sender_of<'a>(header: &'a Header<'a>) -> &'a str {
    header.sender().map_or("(unknown)", |name| name.as_str())
}

/// D-Bus error name for a settings dot-path that does not resolve.
pub const NOT_FOUND_ERROR: &str = "com.mica.micad1.Error.NotFound";
/// D-Bus error name for a settings dot-path that exists but rejects writes.
pub const READ_ONLY_ERROR: &str = "com.mica.micad1.Error.ReadOnly";

/// Reply error of the settings methods.
#[derive(Debug)]
pub(super) enum SettingsFault {
    /// [`NOT_FOUND_ERROR`], from [`SettingsError::NotFound`].
    NotFound(String),
    /// [`READ_ONLY_ERROR`], from [`SettingsError::ReadOnly`].
    ReadOnly(String),
    /// Everything else, under its standard fdo name.
    Fdo(fdo::Error),
}

impl zbus::DBusError for SettingsFault {
    fn name(&self) -> zbus::names::ErrorName<'_> {
        match self {
            Self::NotFound(_) => zbus::names::ErrorName::from_static_str_unchecked(NOT_FOUND_ERROR),
            Self::ReadOnly(_) => zbus::names::ErrorName::from_static_str_unchecked(READ_ONLY_ERROR),
            Self::Fdo(err) => err.name(),
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            Self::NotFound(message) | Self::ReadOnly(message) => Some(message),
            Self::Fdo(err) => err.description(),
        }
    }

    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<zbus::message::Message> {
        match self {
            Self::Fdo(err) => err.create_reply(call),
            // The reply body is the description string, the same single-`s`
            // shape every fdo error reply carries.
            _ => zbus::message::Message::error(call, self.name())?
                .build(&self.description().unwrap_or_default()),
        }
    }
}

/// Map settings errors onto D-Bus error names.
pub(super) fn to_bus_error(err: SettingsError) -> SettingsFault {
    match err {
        // What the product does not carry is not there, like a path that
        // names nothing.
        SettingsError::NotFound(_) | SettingsError::NotServed { .. } => {
            SettingsFault::NotFound(err.to_string())
        }
        SettingsError::ReadOnly(_) => SettingsFault::ReadOnly(err.to_string()),
        SettingsError::Validation { .. } => {
            SettingsFault::Fdo(fdo::Error::InvalidArgs(err.to_string()))
        }
        // The configuration medium being gone is an IO condition and not a
        // malformed request: the caller asked for something reasonable and the
        // device cannot reach the store. The message already names the mount.
        SettingsError::Io(_) | SettingsError::Unavailable { .. } => {
            SettingsFault::Fdo(fdo::Error::IOError(err.to_string()))
        }
        SettingsError::Parse(_) | SettingsError::SchemaVersion(_) => {
            SettingsFault::Fdo(fdo::Error::Failed(err.to_string()))
        }
    }
}

/// Map an update-action refusal onto a D-Bus error: policy refusals carry
/// `AccessDenied` (apid maps it to 409 — the request was well-formed and the
/// device said no), malformed requests carry `InvalidArgs` (422), and
/// busy/unavailable stay `Failed`.
pub(super) fn refusal_to_fdo(refusal: Refusal) -> fdo::Error {
    match refusal {
        Refusal::Policy(message) => fdo::Error::AccessDenied(message),
        Refusal::Invalid(message) => fdo::Error::InvalidArgs(message),
        Refusal::Busy(message) | Refusal::Unavailable(message) => fdo::Error::Failed(message),
    }
}

/// Map a transient-password failure onto a D-Bus error.
pub(super) fn transient_to_fdo(err: anyhow::Error) -> fdo::Error {
    fdo::Error::Failed(format!("set transient root password: {err:#}"))
}
