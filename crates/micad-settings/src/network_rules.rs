//! The rules a `network` subtree is held to, stated once.
//!
//! micad runs them on every apply, because the settings file is writable
//! without apid; apid runs them on its write path, so an operator reads which
//! field is wrong instead of a failed bus call. Both call these functions, so
//! the two cannot disagree about what a declaration is.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::{IfaceSettings, WireguardPeer};

/// Linux `IFNAMSIZ` minus the terminator: the longest name an interface can
/// actually have.
pub const MAX_IFACE_LEN: usize = 15;

/// Refuse an interface name the kernel could not have and a renderer must not
/// see; `field` names the setting in the message.
pub fn check_iface_name(field: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{field} is empty"));
    }
    if name.len() > MAX_IFACE_LEN {
        return Err(format!(
            "{field} {name:?} is longer than {MAX_IFACE_LEN} characters"
        ));
    }
    if name == "." || name == ".." {
        return Err(format!("{field} {name:?} is not a name"));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    {
        return Err(format!(
            "{field} {name:?} contains a character an interface name cannot have"
        ));
    }
    Ok(())
}

/// True when `value` parses as an IP address with an optional `/prefix`.
#[must_use]
pub fn is_ip_or_cidr(value: &str) -> bool {
    let (addr, prefix) = match value.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (value, None),
    };
    let Ok(addr) = addr.parse::<IpAddr>() else {
        return false;
    };
    match prefix {
        None => true,
        Some(prefix) => prefix
            .parse::<u8>()
            .is_ok_and(|p| p <= if addr.is_ipv4() { 32 } else { 128 }),
    }
}

/// True when `value` is the `host:port` a peer's `endpoint` has to be.
#[must_use]
pub fn is_host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    if port.parse::<u16>().is_err() {
        return false;
    }
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner.parse::<Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Whether `value` is a WireGuard key: 32 bytes in canonical, padded,
/// standard-alphabet base64, the spelling `wg` itself reads and writes.
#[must_use]
pub fn is_wireguard_key(value: &str) -> bool {
    STANDARD.decode(value).is_ok_and(|bytes| bytes.len() == 32)
}

/// Refuse a tunnel's peers the renderer must not see.
///
/// A rejected peer is named by its index and never by its key: an operator who
/// pasted a *private* key into the field would otherwise find it in the error
/// text.
pub fn validate_peers(iface: &str, peers: &[WireguardPeer]) -> Result<(), String> {
    for (index, peer) in peers.iter().enumerate() {
        if !is_wireguard_key(&peer.public_key) {
            return Err(format!(
                "network.{iface} peer {index} has a public key that is not a WireGuard key: it must be 32 bytes spelled in base64"
            ));
        }
        for allowed in &peer.allowed_ips {
            if !is_ip_or_cidr(allowed) {
                return Err(format!(
                    "network.{iface} peer {index} allowed IP {allowed:?} is not an IP address or CIDR"
                ));
            }
        }
        if let Some(endpoint) = &peer.endpoint
            && !is_host_port(endpoint)
        {
            return Err(format!(
                "network.{iface} peer {index} endpoint {endpoint:?} is not host:port"
            ));
        }
    }
    Ok(())
}

/// Refuse relations between entries that cannot be built: a VLAN whose parent
/// is not declared, a bridge port that is not declared, carries addressing of
/// its own, or is claimed by two bridges.
///
/// Over the whole subtree, because every rule here is about two entries at
/// once: editing `eth1` to take an address is refused when `br0` claims it.
pub fn validate_topology(network: &BTreeMap<String, IfaceSettings>) -> Result<(), String> {
    let mut claimed_by: BTreeMap<&str, &str> = BTreeMap::new();
    for (iface, cfg) in network {
        if let Some(vlan) = &cfg.vlan
            && !network.contains_key(&vlan.parent)
        {
            return Err(format!(
                "network.{iface} has VLAN parent {:?}, which is not a declared network entry",
                vlan.parent
            ));
        }
        let Some(bridge) = &cfg.bridge else {
            continue;
        };
        for port in &bridge.ports {
            let Some(port_cfg) = network.get(port) else {
                return Err(format!(
                    "network.{iface} has bridge port {port:?}, which is not a declared network entry"
                ));
            };
            if port_cfg.dhcp || port_cfg.static_.is_some() {
                return Err(format!(
                    "network.{port} is a port of bridge {iface} and must not carry addressing of its own"
                ));
            }
            if let Some(other) = claimed_by.insert(port, iface) {
                return Err(format!(
                    "network.{port} is claimed as a port by both bridge {other} and bridge {iface}"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_32_bytes_of_padded_base64() {
        assert!(is_wireguard_key(&STANDARD.encode([7u8; 32])));
        assert!(!is_wireguard_key(&STANDARD.encode([7u8; 31])));
        assert!(!is_wireguard_key(&STANDARD.encode([7u8; 33])));
        let unpadded = STANDARD.encode([7u8; 32]);
        assert!(!is_wireguard_key(unpadded.trim_end_matches('=')));
        assert!(!is_wireguard_key("not base64 at all"));
        assert!(!is_wireguard_key(""));
    }

    #[test]
    fn an_endpoint_is_a_host_and_a_port_or_it_is_nothing() {
        assert!(is_host_port("vpn.example.net:51820"));
        assert!(is_host_port("10.0.0.1:51820"));
        assert!(is_host_port("[fd00::1]:51820"));
        assert!(!is_host_port("vpn.example.net"));
        assert!(!is_host_port("vpn.example.net:"));
        assert!(!is_host_port("vpn.example.net:70000"));
        assert!(!is_host_port(":51820"));
        assert!(!is_host_port("fd00::1:51820"));
        assert!(!is_host_port("vpn example.net:51820"));
    }

    #[test]
    fn an_address_takes_a_prefix_in_its_own_family_range() {
        assert!(is_ip_or_cidr("10.0.0.1"));
        assert!(is_ip_or_cidr("10.0.0.0/32"));
        assert!(!is_ip_or_cidr("10.0.0.0/33"));
        assert!(is_ip_or_cidr("fd00::/128"));
        assert!(!is_ip_or_cidr("fd00::/129"));
        assert!(!is_ip_or_cidr("host/24"));
    }
}
