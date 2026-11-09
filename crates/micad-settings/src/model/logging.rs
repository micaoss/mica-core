//! Log settings: whether the log is kept on DATA, its cap, and a collector it
//! is forwarded to.
//!
//! micad renders them into Base's `/var/lib/mica/log/log.env`, and Base's
//! `mica-log-env` decides what applies; a value left unset is left out of that
//! file, so the image's and the board's defaults decide it.

use std::net::IpAddr;

/// The `logging` subtree.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingSettings {
    /// Whether the log is kept on DATA across boots; unset follows the image
    /// and the board (on, but off on raw NAND).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    /// The cap of the log on DATA, in KiB; unset is Base's default (a quarter
    /// of DATA/var's limit, at most 32 MiB).
    #[serde(rename = "maxSizeKiB", skip_serializing_if = "Option::is_none")]
    pub max_size_kib: Option<u32>,
    /// A syslog collector the log is forwarded to.
    pub remote: RemoteLogSettings,
}

/// A syslog collector. It is kept while switched off, so switching back on
/// restores it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteLogSettings {
    /// Whether the log is forwarded.
    pub enabled: bool,
    /// The collector's IP address, IPv4 or IPv6; Base forwards to addresses
    /// only.
    pub address: String,
    /// The collector's port.
    pub port: u16,
    /// How the log reaches it.
    pub protocol: RemoteLogProtocol,
}

impl Default for RemoteLogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            address: String::new(),
            port: 514,
            protocol: RemoteLogProtocol::Udp,
        }
    }
}

/// The transport to a collector: plain syslog, never encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteLogProtocol {
    #[default]
    Udp,
    /// systemd-netlogd only: busybox syslogd forwards over UDP.
    Tcp,
}

impl RemoteLogProtocol {
    /// The value `log.env` spells it with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }
}

/// The smallest cap Base takes, in KiB.
pub const MIN_LOG_MAX_KIB: u32 = 1024;
/// The largest cap any device takes, in KiB: half of DATA/var's largest
/// limit. The device's own bound is smaller on a smaller DATA, and Base's
/// `mica-log-env` holds the cap to it.
pub const MAX_LOG_MAX_KIB: u32 = 131_072;

/// The `logging` subtree's whole write rule, called by [`Settings::set`].
///
/// # Errors
///
/// Returns the sentence the refusal carries.
///
/// [`Settings::set`]: crate::Settings::set
pub fn validate_logging(logging: &LoggingSettings) -> Result<(), String> {
    if let Some(cap) = logging.max_size_kib
        && !(MIN_LOG_MAX_KIB..=MAX_LOG_MAX_KIB).contains(&cap)
    {
        return Err(format!(
            "logging.maxSizeKiB is {cap}; a cap is {MIN_LOG_MAX_KIB} to {MAX_LOG_MAX_KIB} KiB, \
             and at most half of DATA/var's limit on this device"
        ));
    }
    let remote = &logging.remote;
    if !remote.address.is_empty() && remote.address.parse::<IpAddr>().is_err() {
        return Err(format!(
            "logging.remote.address {:?} is not an IP address: the log is forwarded to an \
             IPv4 or IPv6 address, never to a host name",
            remote.address
        ));
    }
    if remote.enabled && remote.address.is_empty() {
        return Err("forwarding is on, but logging.remote.address names no collector".into());
    }
    if remote.port == 0 {
        return Err("logging.remote.port is 0; a collector's port is 1 to 65535".into());
    }
    Ok(())
}

/// Refuse what the product's init cannot forward: busybox syslogd, an OpenRC
/// root's, forwards over UDP only.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_logging_for_init(
    logging: &LoggingSettings,
    init: crate::Init,
) -> Result<(), String> {
    if init == crate::Init::Openrc && logging.remote.protocol == RemoteLogProtocol::Tcp {
        return Err(
            "logging.remote.protocol tcp: an OpenRC device forwards its log over UDP only".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(enabled: bool, address: &str, port: u16) -> LoggingSettings {
        LoggingSettings {
            remote: RemoteLogSettings {
                enabled,
                address: address.into(),
                port,
                protocol: RemoteLogProtocol::Udp,
            },
            ..LoggingSettings::default()
        }
    }

    #[test]
    fn unset_values_are_left_out_of_the_document() {
        let json = serde_json::to_value(LoggingSettings::default()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "remote": {"enabled": false, "address": "", "port": 514, "protocol": "udp"}
            })
        );
        let read: LoggingSettings =
            serde_json::from_value(serde_json::json!({"persistent": false, "maxSizeKiB": 2048}))
                .unwrap();
        assert_eq!(read.persistent, Some(false));
        assert_eq!(read.max_size_kib, Some(2048));
    }

    #[test]
    fn the_cap_and_the_collector_are_held_to_what_base_takes() {
        assert!(validate_logging(&LoggingSettings::default()).is_ok());
        for cap in [MIN_LOG_MAX_KIB, MAX_LOG_MAX_KIB] {
            let logging = LoggingSettings {
                max_size_kib: Some(cap),
                ..LoggingSettings::default()
            };
            assert!(validate_logging(&logging).is_ok(), "{cap}");
        }
        for cap in [0, MIN_LOG_MAX_KIB - 1, MAX_LOG_MAX_KIB + 1] {
            let logging = LoggingSettings {
                max_size_kib: Some(cap),
                ..LoggingSettings::default()
            };
            assert!(validate_logging(&logging).is_err(), "{cap}");
        }
        assert!(validate_logging(&remote(true, "192.0.2.10", 514)).is_ok());
        assert!(validate_logging(&remote(true, "2001:db8::1", 6514)).is_ok());
        let error = validate_logging(&remote(true, "logs.example.com", 514)).unwrap_err();
        assert!(error.contains("never to a host name"), "{error}");
        assert!(validate_logging(&remote(true, "[2001:db8::1]", 514)).is_err());
        assert!(validate_logging(&remote(true, "", 514)).is_err());
        assert!(validate_logging(&remote(false, "", 514)).is_ok());
        assert!(validate_logging(&remote(true, "192.0.2.10", 0)).is_err());
    }

    #[test]
    fn an_openrc_device_forwards_over_udp_only() {
        let mut logging = remote(true, "192.0.2.10", 514);
        logging.remote.protocol = RemoteLogProtocol::Tcp;
        assert!(validate_logging_for_init(&logging, crate::Init::Systemd).is_ok());
        let error = validate_logging_for_init(&logging, crate::Init::Openrc).unwrap_err();
        assert!(error.contains("UDP only"), "{error}");
        logging.remote.protocol = RemoteLogProtocol::Udp;
        assert!(validate_logging_for_init(&logging, crate::Init::Openrc).is_ok());
    }
}
