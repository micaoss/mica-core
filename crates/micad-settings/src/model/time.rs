//! Time settings: NTP servers and the presentation timezone.

/// Time synchronization and presentation-timezone settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeSettings {
    /// Managed NTP servers.
    pub ntp: NtpSettings,
    /// IANA timezone name used for presentation and explicitly local
    /// schedules; it never moves the machine clock off UTC.
    pub timezone: String,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            ntp: NtpSettings::default(),
            timezone: "UTC".to_string(),
        }
    }
}

/// The managed NTP server list.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NtpSettings {
    /// Server names or addresses, rendered in order into timesyncd's runtime
    /// `NTP=` list. Empty means the image's fallback pool is used — an empty
    /// list is "no operator override", not "no time synchronization".
    pub servers: Vec<String>,
}

/// The most servers one `time.ntp.servers` list may carry.
///
/// timesyncd polls one selected server at a time and steps through the list
/// only on failure, so a longer list buys redundancy, not accuracy; eight is
/// well past any real deployment and keeps the rendered `NTP=` line bounded.
pub const MAX_NTP_SERVERS: usize = 8;

/// RFC 1035's bound on a full domain name, which also covers any IP literal.
pub(super) const MAX_NTP_SERVER_LEN: usize = 253;

/// Longest IANA zone name accepted; the longest real one is around 32 bytes.
pub(super) const MAX_TIMEZONE_LEN: usize = 64;

/// Refuse a `time.ntp.servers` list timesyncd's `NTP=` line cannot carry.
pub fn validate_ntp_servers(servers: &[String]) -> Result<(), String> {
    if servers.len() > MAX_NTP_SERVERS {
        return Err(format!(
            "at most {MAX_NTP_SERVERS} NTP servers are supported; timesyncd only ever polls one \
             and steps through the rest on failure"
        ));
    }
    for (index, server) in servers.iter().enumerate() {
        if server.is_empty() {
            return Err(format!("NTP server {} is empty", index + 1));
        }
        if server.len() > MAX_NTP_SERVER_LEN {
            return Err(format!(
                "NTP server {server:?} is longer than {MAX_NTP_SERVER_LEN} characters"
            ));
        }
        if !server
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
        {
            return Err(format!(
                "NTP server {server:?} contains a character a host name or IP address cannot \
                 have; use ASCII letters, digits, '.', '-' or ':'"
            ));
        }
        if servers[..index].contains(server) {
            return Err(format!("NTP server {server:?} is listed twice"));
        }
    }
    Ok(())
}

/// Refuse a `time.timezone` value that is not an IANA zone name.
pub fn validate_timezone_name(zone: &str) -> Result<(), String> {
    const RULES: &str = "a timezone is an IANA zone name such as \"UTC\" or \"Europe/Berlin\": \
                         '/'-separated components of ASCII letters, digits, '.', '_', '+' and '-'";
    if zone.is_empty() || zone.len() > MAX_TIMEZONE_LEN {
        return Err(format!(
            "{RULES}, between 1 and {MAX_TIMEZONE_LEN} characters"
        ));
    }
    for component in zone.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(RULES.to_string());
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
        {
            return Err(RULES.to_string());
        }
    }
    Ok(())
}

/// The `time` subtree's whole write rule, called by [`Settings::set`].
pub(super) fn validate_time_settings(time: &TimeSettings) -> Result<(), String> {
    validate_ntp_servers(&time.ntp.servers)?;
    validate_timezone_name(&time.timezone)
}
