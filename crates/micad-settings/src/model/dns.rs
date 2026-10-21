//! Global DNS servers and static host records.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};

use super::IfaceSettings;

/// DNS settings that hold for the whole device rather than one interface.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DnsSettings {
    /// Servers every lookup goes to in place of the interfaces' own.
    pub global: GlobalDns,
    /// Names this device resolves itself, from its hosts file.
    #[serde(rename = "staticRecords", skip_serializing_if = "Vec::is_empty")]
    pub static_records: Vec<StaticRecord>,
}

/// The global DNS servers. The list is kept while switched off, so switching
/// back on restores it.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalDns {
    /// Whether the servers replace every interface's.
    pub enabled: bool,
    /// IPv4 server addresses, at most [`MAX_GLOBAL_DNS_SERVERS`].
    pub servers: Vec<String>,
}

/// One name the hosts file maps to an address.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticRecord {
    /// What names the record to the API: 1 to [`MAX_RECORD_ID_LEN`] letters,
    /// digits, `-` and `_`, unique among the records.
    pub id: String,
    /// The host name, without a wildcard.
    pub hostname: String,
    /// The IPv4 address it resolves to.
    pub address: String,
    /// A note for people, written after the record as a comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// The most global DNS servers.
pub const MAX_GLOBAL_DNS_SERVERS: usize = 3;
/// The most static records.
pub const MAX_STATIC_RECORDS: usize = 100;
/// The longest record comment, in characters.
pub const MAX_RECORD_COMMENT_CHARS: usize = 64;
/// The longest record id, in bytes.
pub const MAX_RECORD_ID_LEN: usize = 32;

/// Whether `name` is a host name: dot-separated labels of letters, digits and
/// inner hyphens, each at most 63 bytes, at most 253 in all.
#[must_use]
pub fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// The addresses the interfaces declare statically.
fn own_addresses(network: &BTreeMap<String, IfaceSettings>) -> BTreeSet<IpAddr> {
    network
        .values()
        .filter_map(|cfg| cfg.static_.as_ref())
        .filter_map(|static_| static_.address.split('/').next()?.parse().ok())
        .collect()
}

/// Refuse a `dns` subtree no resolver should be given: a global server that
/// is not IPv4, is loopback or unspecified, is one of the interfaces' own
/// addresses or is listed twice, more than [`MAX_GLOBAL_DNS_SERVERS`], and
/// none while switched on; a record whose id is not 1 to
/// [`MAX_RECORD_ID_LEN`] of `[A-Za-z0-9_-]` or is listed twice, whose name is
/// not a host name, is `localhost` or this device's `hostname` or is listed
/// twice, whose address is not IPv4, or whose comment is long or carries a
/// control character, and more than [`MAX_STATIC_RECORDS`].
pub fn validate_dns(
    dns: &DnsSettings,
    hostname: &str,
    network: &BTreeMap<String, IfaceSettings>,
) -> Result<(), String> {
    let global = &dns.global;
    if global.servers.len() > MAX_GLOBAL_DNS_SERVERS {
        return Err(format!(
            "dns.global holds more than {MAX_GLOBAL_DNS_SERVERS} servers"
        ));
    }
    if global.enabled && global.servers.is_empty() {
        return Err("dns.global is switched on with no server".to_string());
    }
    let own = own_addresses(network);
    let mut seen = BTreeSet::new();
    for server in &global.servers {
        let Ok(addr) = server.parse::<Ipv4Addr>() else {
            return Err(format!(
                "dns.global server {server:?} is not an IPv4 address"
            ));
        };
        if addr.is_loopback() || addr.is_unspecified() {
            return Err(format!(
                "dns.global server {addr} is this device itself, not a server it can ask"
            ));
        }
        if own.contains(&IpAddr::V4(addr)) {
            return Err(format!(
                "dns.global server {addr} is an address of this device's own interfaces"
            ));
        }
        if !seen.insert(addr) {
            return Err(format!("dns.global lists server {addr} twice"));
        }
    }
    if dns.static_records.len() > MAX_STATIC_RECORDS {
        return Err(format!(
            "dns.staticRecords holds more than {MAX_STATIC_RECORDS} records"
        ));
    }
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for record in &dns.static_records {
        let id_ok = !record.id.is_empty()
            && record.id.len() <= MAX_RECORD_ID_LEN
            && record
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if !id_ok {
            return Err(format!(
                "dns.staticRecords id {:?} is not 1 to {MAX_RECORD_ID_LEN} letters, digits, `-` or `_`",
                record.id
            ));
        }
        if !ids.insert(record.id.as_str()) {
            return Err(format!("dns.staticRecords lists id {} twice", record.id));
        }
        let name = record.hostname.to_ascii_lowercase();
        if !is_host_name(&name) {
            return Err(format!(
                "dns.staticRecords name {:?} is not a host name; a wildcard is not one either",
                record.hostname
            ));
        }
        if name == "localhost" || name.ends_with(".localhost") {
            return Err(format!(
                "dns.staticRecords name {:?} is localhost, which the device always resolves itself",
                record.hostname
            ));
        }
        if name == hostname.to_ascii_lowercase() {
            return Err(format!(
                "dns.staticRecords name {:?} is this device's own hostname",
                record.hostname
            ));
        }
        if record.address.parse::<Ipv4Addr>().is_err() {
            return Err(format!(
                "dns.staticRecords address {:?} for {} is not an IPv4 address",
                record.address, record.hostname
            ));
        }
        if let Some(comment) = &record.comment
            && (comment.chars().count() > MAX_RECORD_COMMENT_CHARS
                || comment.chars().any(char::is_control))
        {
            return Err(format!(
                "dns.staticRecords comment for {} is longer than {MAX_RECORD_COMMENT_CHARS} characters or carries a control character",
                record.hostname
            ));
        }
        if !names.insert(name) {
            return Err(format!("dns.staticRecords lists {} twice", record.hostname));
        }
    }
    Ok(())
}
