//! The rules a `network` write is held to beyond [`crate::network_rules`]'s:
//! each carried entry's own address, gateway, routes and DNS servers, and
//! the rules between entries (overlapping subnets, default-route metrics,
//! VLAN ids). apid runs them on its network routes and micad on a BLE
//! `setNetwork`, so both refuse the same map with the same codes.

use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::{
    IfaceKind, IfaceSettings, NetworkViolation, check_iface_name, is_ip_or_cidr,
    topology_violations, validate_can, validate_peers,
};

pub fn valid_ipv4(address: &str) -> bool {
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
pub fn valid_cidr(cidr: &str) -> bool {
    let Some((address, prefix)) = cidr.split_once('/') else {
        return false;
    };
    valid_ipv4(address)
        && !prefix.is_empty()
        && prefix.len() <= 2
        && prefix.bytes().all(|b| b.is_ascii_digit())
        && prefix.parse::<u8>().is_ok_and(|value| value <= 32)
}

/// A static address an entry holds is an IPv4 CIDR. DHCP off with an empty
/// address is an entry with no addressing, as a bridge port has, not an
/// error.
///
/// # Errors
/// The address is not an IPv4 CIDR.
pub fn validate_static_address(dhcp: bool, address: &str) -> Result<(), &'static str> {
    if !dhcp && !address.is_empty() && !valid_cidr(address) {
        return Err("Static address must be IPv4 CIDR notation, e.g. 192.168.1.10/24.");
    }
    Ok(())
}

/// The routing rules of one entry, as the field and the message of the first
/// one it breaks.
///
/// Two refusals, both about configurations networkd would accept and nothing
/// could use:
///
/// - a route whose destination is not a network, or whose next hop is not an
///   address;
/// - a second default route, declared as a `0.0.0.0/0` route beside a
///   `static.gateway` that already is one.
///
/// And `dns`, which replaces a lease's servers, only beside DHCP.
pub fn routing_problem(cfg: &IfaceSettings) -> Option<(&'static str, String)> {
    let has_gateway = cfg
        .static_
        .as_ref()
        .is_some_and(|static_| static_.gateway.is_some());
    for route in &cfg.routes {
        if !is_ip_or_cidr(&route.destination) {
            return Some((
                "routes",
                format!(
                    "the route destination {:?} is not a network in CIDR notation, such as `10.20.0.0/16`",
                    route.destination
                ),
            ));
        }
        if let Some(gateway) = &route.gateway
            && gateway.parse::<IpAddr>().is_err()
        {
            return Some((
                "routes",
                format!(
                    "the route via {gateway:?} does not name an address; omit the gateway for an on-link route"
                ),
            ));
        }
        if is_default_destination(&route.destination) && has_gateway {
            return Some((
                "routes",
                format!(
                    "this entry declares a default route twice: once as `static.gateway` and once as the route to {:?}. Keep one",
                    route.destination
                ),
            ));
        }
    }
    if !cfg.dns.is_empty() {
        if !cfg.dhcp || cfg.static_.is_some() {
            return Some(("dns",
                "`dns` replaces the servers of a DHCP lease, so it needs `dhcp`; a static entry names its servers in `static.dns`"
                    .to_string(),
            ));
        }
        for dns in &cfg.dns {
            if dns.parse::<IpAddr>().is_err() {
                return Some(("dns", format!("the DNS server {dns:?} is not an address")));
            }
        }
    }
    None
}

/// Whether a destination is the default route, in either family.
pub fn is_default_destination(destination: &str) -> bool {
    matches!(destination, "0.0.0.0/0" | "::/0")
}

/// The reconciler's peer, CAN and topology rules over the whole candidate
/// subtree: every topology rule is about two entries at once, so editing
/// `eth1` to take an address is refused when `br0` claims it.
pub fn relational_violations(entries: &BTreeMap<String, IfaceSettings>) -> Vec<NetworkViolation> {
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
    found.extend(topology_violations(entries));
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
pub fn entry_violations(iface: &str, cfg: &IfaceSettings) -> Vec<NetworkViolation> {
    let mut found = Vec::new();
    let mut push = |field: &'static str, code: &'static str, params, message: String| {
        found.push(NetworkViolation::new(iface, field, code, params, message));
    };
    if !check_iface_name("", iface).is_ok() {
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
    if !cfg.internet() {
        if matches!(cfg.kind, IfaceKind::Wireguard | IfaceKind::Can) {
            push(
                "internet",
                "validation_failed",
                serde_json::json!({}),
                "`internet` is for physical, bridge and VLAN entries".to_string(),
            );
        }
        if let Some(gateway) = cfg
            .static_
            .as_ref()
            .and_then(|static_| static_.gateway.as_deref())
        {
            push(
                "static.gateway",
                "gateway_not_allowed",
                serde_json::json!({ "gateway": gateway }),
                format!(
                    "an interface with `internet` false has no gateway; {gateway} would be its default route"
                ),
            );
        }
        if cfg
            .routes
            .iter()
            .any(|route| route.destination == "0.0.0.0/0")
        {
            push(
                "routes",
                "gateway_not_allowed",
                serde_json::json!({}),
                "an interface with `internet` false carries no default route".to_string(),
            );
        }
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
/// (`vlan_duplicate`), two tunnels listen on one port (`port_in_use`).
pub fn write_violations(
    entries: &BTreeMap<String, IfaceSettings>,
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
    let ports: Vec<(&str, u16)> = entries
        .iter()
        .filter_map(|(iface, cfg)| Some((iface.as_str(), cfg.wireguard.as_ref()?.listen_port?)))
        .collect();
    for (target, other) in collisions(&ports, carried, |a, b| a == b) {
        let ((iface, port), other) = (ports[target], ports[other].0);
        found.push(NetworkViolation::new(
            iface,
            "wireguard.listenPort",
            "port_in_use",
            serde_json::json!({ "name": other, "port": port }),
            format!("network.{other} already listens on UDP port {port}"),
        ));
    }
    found
}

/// The entries that name `iface`: as a VLAN parent or a bridge port.
pub fn references_to(entries: &BTreeMap<String, IfaceSettings>, iface: &str) -> Vec<String> {
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
pub fn map_violations(
    entries: &BTreeMap<String, IfaceSettings>,
    carried: &[&str],
) -> Vec<NetworkViolation> {
    let mut found = relational_violations(entries);
    for (iface, cfg) in entries {
        if carried.contains(&iface.as_str()) {
            found.extend(entry_violations(iface, cfg));
        }
    }
    found.extend(write_violations(entries, carried));
    found
}
