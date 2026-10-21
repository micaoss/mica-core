//! Syntactic checks shared by the network and system routes.

use micad_settings::{IfaceSettings, MIN_ADMIN_PASSWORD_LEN, quote_path_segment};

use super::*;

pub(super) const HOSTNAME_RULES: &str =
    "Hostname must be 1-63 letters, digits or hyphens and must not start or end with a hyphen.";

/// Whether an admin password is under [`MIN_ADMIN_PASSWORD_LEN`].
///
/// Bytes and not characters: `str::len` is the byte length, so a password of
/// eight non-ASCII characters is over the floor. Named rather than
/// inlined so the comparison, and not only the number, has one spelling.
///
/// The number itself is [`micad_settings::MIN_ADMIN_PASSWORD_LEN`], the one
/// statement of the rule: `micad-settings` is the crate apid and micad both
/// link, so a credential this device accepts is one it keeps accepting.
pub(super) fn password_under_floor(password: &str) -> bool {
    password.len() < MIN_ADMIN_PASSWORD_LEN
}

/// The interface-name rule micad holds every `network` key to
/// ([`micad_settings::check_iface_name`]): 1 to 15 letters, digits, `.`, `_`,
/// `-` or `:`, and not `.` or `..`. The same function, so apid cannot accept a
/// name micad refuses or refuse one it accepts.
pub(super) fn valid_iface_name(name: &str) -> bool {
    micad_settings::check_iface_name("", name).is_ok()
}

pub(super) fn valid_ipv4(address: &str) -> bool {
    let octets: Vec<&str> = address.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|octet| {
            !octet.is_empty()
                && octet.len() <= 3
                && octet.bytes().all(|b| b.is_ascii_digit())
                && octet.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

/// Minimal `a.b.c.d/len` shape with in-range octets and prefix length.
pub(super) fn valid_cidr(cidr: &str) -> bool {
    let Some((address, prefix)) = cidr.split_once('/') else {
        return false;
    };
    valid_ipv4(address)
        && !prefix.is_empty()
        && prefix.len() <= 2
        && prefix.bytes().all(|b| b.is_ascii_digit())
        && prefix.parse::<u8>().is_ok_and(|value| value <= 32)
}

/// `^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$`
pub(super) fn valid_hostname(name: &str) -> bool {
    let bytes = name.as_bytes();
    matches!(bytes.len(), 1..=63)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes[0] != b'-'
        && bytes[bytes.len() - 1] != b'-'
}

/// Interface form validation shared by `/network` and the setup wizard.
///
/// DHCP off with an empty address is an interface with **no** addressing, not
/// an error: a bridge port *must* carry neither `dhcp` nor `static`
/// (`validate_network` in `micad/src/reconciler/network.rs`), and it must be a
/// declared entry before a bridge may name it, so a pane that insisted on an
/// address made a bridge unbuildable through the form. An address that is
/// present and not a CIDR is still refused.
pub(super) fn validate_iface(iface: &str, dhcp: bool, address: &str) -> Result<(), &'static str> {
    if !valid_iface_name(iface) {
        return Err(
            "Interface name must be 1-15 characters of letters, digits, '.', '_', '-' or ':', and not '.' or '..'.",
        );
    }
    validate_static_address(dhcp, address)
}

/// The address clause of [`validate_iface`], as its own function.
///
/// It is factored out rather than copied because the typed network routes
/// have to run this rule too and must not be able to disagree with the forms
/// about what an address is: `PUT /api/v1/network/{iface}` took an address the
/// kernel cannot parse and answered 204 until this was factored out. Those
/// routes cannot call `validate_iface`
/// itself, because they check the name with [`check_iface_name`] and would
/// otherwise carry two spellings of the name refusal.
///
/// The condition is unchanged and is not widened: DHCP off with an empty
/// address is an interface with no addressing, for the reason
/// [`validate_iface`] states.
pub(super) fn validate_static_address(dhcp: bool, address: &str) -> Result<(), &'static str> {
    if !dhcp && !address.is_empty() && !valid_cidr(address) {
        return Err("Static address must be IPv4 CIDR notation, e.g. 192.168.1.10/24.");
    }
    Ok(())
}

/// The settings path of `iface`'s entry, with the name quoted when it carries
/// a dot: a VLAN named `eth0.100` is `network."eth0.100"`, not three segments.
pub(super) fn iface_settings_path(iface: &str) -> String {
    format!("network.{}", quote_path_segment(iface))
}

// The reconciler's own rules, run here first so the operator reads which field
// is wrong instead of a failed bus call.
pub(super) use micad_settings::{
    NetworkViolation, is_ip_or_cidr, is_wireguard_key, validate_can, validate_peers,
};

/// The reconciler's peer, CAN and topology rules over the whole candidate
/// subtree: every topology rule is about two entries at once, so editing
/// `eth1` to take an address is refused when `br0` claims it.
pub(super) fn relational_violations(entries: &NetworkEntries) -> Vec<NetworkViolation> {
    let mut found = Vec::new();
    for (iface, cfg) in entries {
        if let Some(wireguard) = &cfg.wireguard
            && let Err(message) = validate_peers(iface, &wireguard.peers)
        {
            found.push(NetworkViolation::new(
                iface,
                "wireguard.peers",
                "validation_failed",
                serde_json::json!({}),
                message,
            ));
        }
        if let Err(message) = validate_can(iface, cfg) {
            found.push(NetworkViolation::new(
                iface,
                "can",
                "validation_failed",
                serde_json::json!({}),
                message,
            ));
        }
    }
    found.extend(micad_settings::topology_violations(entries));
    found
}

/// An IPv4 CIDR as its address and prefix length.
fn ipv4_cidr(cidr: &str) -> Option<(u32, u32)> {
    if !valid_cidr(cidr) {
        return None;
    }
    let (address, prefix) = cidr.split_once('/')?;
    Some((
        u32::from(address.parse::<std::net::Ipv4Addr>().ok()?),
        prefix.parse().ok()?,
    ))
}

fn mask(prefix: u32) -> u32 {
    u32::MAX.checked_shl(32 - prefix).unwrap_or(0)
}

/// The static address an entry holds, as an IPv4 CIDR.
fn static_cidr(cfg: &IfaceSettings) -> Option<(u32, u32)> {
    if cfg.dhcp {
        return None;
    }
    ipv4_cidr(&cfg.static_.as_ref()?.address)
}

/// The rules of one entry the request carries: its name, its static address
/// and gateway, its routes and its DNS servers.
pub(super) fn entry_violations(iface: &str, cfg: &IfaceSettings) -> Vec<NetworkViolation> {
    let mut found = Vec::new();
    let mut push = |field: &'static str, code: &'static str, params, message: String| {
        found.push(NetworkViolation::new(iface, field, code, params, message));
    };
    if !valid_iface_name(iface) {
        push(
            "name",
            "name_invalid",
            serde_json::json!({ "name": iface }),
            "an interface name is 1 to 15 characters of letters, digits, `.`, `_`, `-` or `:`, and \
             not `.` or `..`"
                .to_string(),
        );
    }
    let address = cfg
        .static_
        .as_ref()
        .map_or("", |static_| static_.address.as_str());
    if let Err(message) = validate_static_address(cfg.dhcp, address) {
        push(
            "static.address",
            "validation_failed",
            serde_json::json!({}),
            message.to_string(),
        );
    }
    if let (Some((own, prefix)), Some(gateway)) = (
        static_cidr(cfg),
        cfg.static_
            .as_ref()
            .and_then(|static_| static_.gateway.as_deref()),
    ) && let Ok(gateway_address) = gateway.parse::<std::net::Ipv4Addr>()
        && u32::from(gateway_address) & mask(prefix) != own & mask(prefix)
    {
        push(
            "static.gateway",
            "gateway_out_of_subnet",
            serde_json::json!({ "gateway": gateway, "address": address }),
            format!("the gateway {gateway} is not in the subnet of {address}"),
        );
    }
    if let Some((field, message)) = routing_problem(cfg) {
        push(field, "validation_failed", serde_json::json!({}), message);
    }
    found
}

/// Each colliding pair among `items` (in order) reported once on the entry
/// the request carries: the later of the two when it is carried, else the
/// earlier. A pair of entries neither of which the request carries is not
/// this request's fault. Each item is reported at most once.
fn collisions<T>(
    items: &[(&str, T)],
    carried: &[&str],
    collide: impl Fn(&T, &T) -> bool,
) -> Vec<(usize, usize)> {
    let mut reported = vec![false; items.len()];
    let mut found = Vec::new();
    for later in 0..items.len() {
        for earlier in 0..later {
            if !collide(&items[earlier].1, &items[later].1) {
                continue;
            }
            let (target, other) = if carried.contains(&items[later].0) {
                (later, earlier)
            } else if carried.contains(&items[earlier].0) {
                (earlier, later)
            } else {
                continue;
            };
            if !reported[target] {
                reported[target] = true;
                found.push((target, other));
            }
        }
    }
    found
}

/// The rules between entries that only a write is held to, for the entries
/// in `carried`: two subnets overlap (`address_overlap`), two default routes
/// share a metric (`metric_duplicate`), a parent carries one VLAN id twice
/// (`vlan_duplicate`).
pub(super) fn write_violations(
    entries: &NetworkEntries,
    carried: &[&str],
) -> Vec<NetworkViolation> {
    let mut found = Vec::new();
    let subnets: Vec<(&str, (u32, u32))> = entries
        .iter()
        .filter_map(|(iface, cfg)| Some((iface.as_str(), static_cidr(cfg)?)))
        .collect();
    for (target, other) in collisions(&subnets, carried, |(a, a_prefix), (b, b_prefix)| {
        let shorter = mask(*a_prefix.min(b_prefix));
        a & shorter == b & shorter
    }) {
        let (iface, other) = (subnets[target].0, subnets[other].0);
        found.push(NetworkViolation::new(
            iface,
            "static.address",
            "address_overlap",
            serde_json::json!({ "name": other }),
            format!("network.{iface} overlaps the subnet of network.{other}"),
        ));
    }
    let metrics: Vec<(&str, u32)> = entries
        .iter()
        .flat_map(|(iface, cfg)| {
            cfg.routes
                .iter()
                .filter(|route| is_default_destination(&route.destination))
                .filter_map(move |route| Some((iface.as_str(), route.metric?)))
        })
        .collect();
    for (target, other) in collisions(&metrics, carried, |a, b| a == b) {
        let ((iface, metric), other) = (metrics[target], metrics[other].0);
        found.push(NetworkViolation::new(
            iface,
            "routes",
            "metric_duplicate",
            serde_json::json!({ "name": other, "metric": metric }),
            format!(
                "the default route metric {metric} of network.{iface} is network.{other}'s too"
            ),
        ));
    }
    let vlans: Vec<(&str, (&str, u16))> = entries
        .iter()
        .filter_map(|(iface, cfg)| {
            let vlan = cfg.vlan.as_ref()?;
            Some((iface.as_str(), (vlan.parent.as_str(), vlan.id)))
        })
        .collect();
    for (target, other) in collisions(&vlans, carried, |a, b| a == b) {
        let ((iface, (parent, id)), other) = (vlans[target], vlans[other].0);
        found.push(NetworkViolation::new(
            iface,
            "vlan.id",
            "vlan_duplicate",
            serde_json::json!({ "parent": parent, "id": id, "name": other }),
            format!("{parent} already carries VLAN {id} (network.{other})"),
        ));
    }
    found
}

/// The entries that name `iface`: as a VLAN parent or a bridge port.
pub(super) fn references_to(entries: &NetworkEntries, iface: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|(_, cfg)| {
            cfg.vlan.as_ref().is_some_and(|vlan| vlan.parent == iface)
                || cfg
                    .bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.ports.iter().any(|port| port == iface))
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Every rule a candidate map breaks, the per-entry ones for the entries in
/// `carried`: the relational rules, then each carried entry's own, then the
/// rules between entries a write is held to.
pub(super) fn map_violations(entries: &NetworkEntries, carried: &[&str]) -> Vec<NetworkViolation> {
    let mut found = relational_violations(entries);
    for (iface, cfg) in entries {
        if carried.contains(&iface.as_str()) {
            found.extend(entry_violations(iface, cfg));
        }
    }
    found.extend(write_violations(entries, carried));
    found
}

// Setup wizard

#[cfg(test)]
mod tests;
