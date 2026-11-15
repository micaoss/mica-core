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

/// The metric of micad's failover default route, the one route that beats
/// every uplink's own while an uplink the kernel would pick is down.
pub const OVERRIDE_METRIC: u32 = 10;

/// The metric of the first static default route; static default routes take
/// 20, 21, ... in name order. A declared default route with a metric below it
/// is refused (`route_metric_reserved`).
pub const STATIC_METRIC_BASE: u32 = 20;

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
    if let Some(metric) = cfg
        .routes
        .iter()
        .filter(|route| is_default_destination(&route.destination))
        .find_map(|route| route.metric.filter(|metric| *metric < STATIC_METRIC_BASE))
    {
        push(
            "routes",
            "route_metric_reserved",
            serde_json::json!({ "metric": metric, "min": STATIC_METRIC_BASE }),
            format!(
                "a default route's metric is {STATIC_METRIC_BASE} or more; {metric} is kept for the device's own routes"
            ),
        );
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
    if let Some(probe) = &cfg.probe {
        found.extend(probe_violations(iface, cfg, probe));
    }
    found
}

/// The most targets one check tries.
pub const MAX_PROBE_TARGETS: usize = 4;

/// The rules of an entry's reachability check, each at its field:
/// `probe_not_allowed` (on an entry that carries no default route, a tunnel
/// or a CAN controller), `probe_targets` (more than four, or one twice),
/// `probe_address`, `probe_method`, `probe_port`, `probe_interval`,
/// `probe_timeout` and `probe_threshold`.
fn probe_violations(
    iface: &str,
    cfg: &IfaceSettings,
    probe: &crate::ProbeConfig,
) -> Vec<NetworkViolation> {
    let mut found = Vec::new();
    let mut push = |field: &'static str, code: &'static str, params, message: String| {
        found.push(NetworkViolation::new(iface, field, code, params, message));
    };
    if !cfg.internet() || matches!(cfg.kind, IfaceKind::Wireguard | IfaceKind::Can) {
        push(
            "probe",
            "probe_not_allowed",
            serde_json::json!({}),
            "a reachability check is for physical, bridge and VLAN entries that may carry the default route"
                .to_string(),
        );
    }
    let targets = &probe.targets;
    let mut seen = std::collections::BTreeSet::new();
    if targets.len() > MAX_PROBE_TARGETS
        || !targets
            .iter()
            .all(|t| seen.insert((t.address.as_str(), t.method.as_str(), t.port)))
    {
        push(
            "probe.targets",
            "probe_targets",
            serde_json::json!({ "max": MAX_PROBE_TARGETS }),
            format!("a check has 1 to {MAX_PROBE_TARGETS} targets, each named once"),
        );
    }
    for target in targets {
        let address = target.address.as_str();
        let unicast = address.parse::<std::net::Ipv4Addr>().is_ok_and(|a| {
            valid_ipv4(address)
                && !(a.is_unspecified() || a.is_loopback() || a.is_multicast() || a.is_broadcast())
        });
        if address != "gateway" && !unicast {
            push(
                "probe.targets",
                "probe_address",
                serde_json::json!({ "address": address }),
                format!("the target {address:?} is not an IPv4 unicast address or `gateway`"),
            );
        }
        match (target.method.as_str(), target.port) {
            ("icmp", Some(port)) => push(
                "probe.targets",
                "probe_port",
                serde_json::json!({ "port": port }),
                "an `icmp` target takes no port".to_string(),
            ),
            ("tcp", Some(0)) => push(
                "probe.targets",
                "probe_port",
                serde_json::json!({ "port": 0 }),
                "a `tcp` target's port is 1 to 65535".to_string(),
            ),
            ("icmp" | "tcp", _) => {}
            (method, _) => push(
                "probe.targets",
                "probe_method",
                serde_json::json!({ "method": method }),
                format!("the method {method:?} is not `icmp` or `tcp`"),
            ),
        }
    }
    let interval = probe.interval_s();
    if !(2..=300).contains(&interval) {
        push(
            "probe.intervalS",
            "probe_interval",
            serde_json::json!({ "min": 2, "max": 300 }),
            format!("the interval {interval} s is not 2 to 300 s"),
        );
    }
    let timeout = probe.timeout_ms();
    if !(200..=10_000).contains(&timeout) || u64::from(timeout) >= u64::from(interval) * 1000 {
        push(
            "probe.timeoutMs",
            "probe_timeout",
            serde_json::json!({ "min": 200, "max": 10_000 }),
            format!("the timeout {timeout} ms is not 200 to 10000 ms and less than the interval"),
        );
    }
    for (field, value, max) in [
        ("probe.failuresToDown", probe.failures_to_down(), 20),
        ("probe.successesToUp", probe.successes_to_up(), 50),
    ] {
        if !(1..=max).contains(&value) {
            push(
                field,
                "probe_threshold",
                serde_json::json!({ "min": 1, "max": max }),
                format!("{value} is not 1 to {max}"),
            );
        }
    }
    found
}

/// The entry as a write stores it: a reachability check enabled with no
/// targets gets [`crate::DEFAULT_PROBE_TARGETS`], written out, so the
/// document says what the device contacts.
pub fn store_probe_defaults(cfg: &mut IfaceSettings) {
    if let Some(probe) = &mut cfg.probe
        && probe.enabled
        && probe.targets.is_empty()
    {
        probe.targets = crate::default_probe_targets();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RouteConfig, StaticConfig};

    // A declared default route below the static base is refused: those
    // metrics are the device's own (micad's failover route is 10).
    #[test]
    fn a_default_route_below_the_static_base_is_refused() {
        let entry = |metric| IfaceSettings {
            static_: Some(StaticConfig {
                address: "10.0.1.10/24".to_string(),
                gateway: None,
                dns: Vec::new(),
            }),
            routes: vec![RouteConfig {
                destination: "0.0.0.0/0".to_string(),
                gateway: Some("10.0.1.1".to_string()),
                metric,
            }],
            ..IfaceSettings::default()
        };
        let codes = |metric| {
            entry_violations("eth0", &entry(metric))
                .into_iter()
                .map(|violation| violation.code)
                .collect::<Vec<_>>()
        };
        assert_eq!(codes(Some(10)), ["route_metric_reserved"]);
        assert_eq!(codes(Some(19)), ["route_metric_reserved"]);
        assert!(codes(Some(20)).is_empty());
        assert!(codes(None).is_empty());
    }

    fn probing(probe: crate::ProbeConfig) -> IfaceSettings {
        IfaceSettings {
            dhcp: true,
            probe: Some(probe),
            ..IfaceSettings::default()
        }
    }

    fn target(address: &str, method: &str, port: Option<u16>) -> crate::ProbeTarget {
        crate::ProbeTarget {
            address: address.to_string(),
            method: method.to_string(),
            port,
        }
    }

    fn probe_codes(cfg: &IfaceSettings) -> Vec<&'static str> {
        entry_violations("eth0", cfg)
            .into_iter()
            .map(|violation| violation.code)
            .collect()
    }

    // Every probe rule refuses at its own field with its own code, and the
    // defaults and the bounds pass.
    #[test]
    fn a_reachability_check_is_held_to_its_rules() {
        let good = crate::ProbeConfig {
            enabled: true,
            targets: vec![
                target("gateway", "icmp", None),
                target("192.0.2.10", "tcp", Some(443)),
            ],
            ..crate::ProbeConfig::default()
        };
        assert!(probe_codes(&probing(good.clone())).is_empty());
        let bounds = crate::ProbeConfig {
            interval_s: Some(2),
            timeout_ms: Some(1999),
            failures_to_down: Some(20),
            successes_to_up: Some(50),
            ..good.clone()
        };
        assert!(probe_codes(&probing(bounds)).is_empty());

        let refused = |probe: crate::ProbeConfig, code: &str| {
            assert_eq!(probe_codes(&probing(probe.clone())), [code], "{probe:?}");
        };
        let with_targets = |targets| crate::ProbeConfig {
            targets,
            ..good.clone()
        };
        refused(
            with_targets(vec![target("192.0.2.1", "icmp", None); 2]),
            "probe_targets",
        );
        refused(
            with_targets(
                (1..=5)
                    .map(|n| target(&format!("192.0.2.{n}"), "icmp", None))
                    .collect(),
            ),
            "probe_targets",
        );
        for address in [
            "127.0.0.1",
            "224.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "fd00::1",
            "example.net",
        ] {
            refused(
                with_targets(vec![target(address, "icmp", None)]),
                "probe_address",
            );
        }
        refused(
            with_targets(vec![target("192.0.2.1", "http", None)]),
            "probe_method",
        );
        refused(
            with_targets(vec![target("192.0.2.1", "icmp", Some(7))]),
            "probe_port",
        );
        refused(
            with_targets(vec![target("192.0.2.1", "tcp", Some(0))]),
            "probe_port",
        );
        refused(
            crate::ProbeConfig {
                interval_s: Some(1),
                timeout_ms: Some(500),
                ..good.clone()
            },
            "probe_interval",
        );
        refused(
            crate::ProbeConfig {
                interval_s: Some(301),
                ..good.clone()
            },
            "probe_interval",
        );
        refused(
            crate::ProbeConfig {
                timeout_ms: Some(199),
                ..good.clone()
            },
            "probe_timeout",
        );
        refused(
            crate::ProbeConfig {
                interval_s: Some(2),
                timeout_ms: Some(2000),
                ..good.clone()
            },
            "probe_timeout",
        );
        refused(
            crate::ProbeConfig {
                failures_to_down: Some(0),
                ..good.clone()
            },
            "probe_threshold",
        );
        refused(
            crate::ProbeConfig {
                successes_to_up: Some(51),
                ..good.clone()
            },
            "probe_threshold",
        );

        for cfg in [
            IfaceSettings {
                internet: Some(false),
                ..probing(good.clone())
            },
            IfaceSettings {
                kind: IfaceKind::Wireguard,
                dhcp: false,
                ..probing(good.clone())
            },
        ] {
            assert!(probe_codes(&cfg).contains(&"probe_not_allowed"), "{cfg:?}");
        }
    }

    // A check enabled with no targets is stored with the two default ones;
    // one switched off, or one with targets, is stored as given.
    #[test]
    fn an_enabled_check_with_no_targets_stores_the_defaults() {
        let mut cfg = probing(crate::ProbeConfig {
            enabled: true,
            ..crate::ProbeConfig::default()
        });
        store_probe_defaults(&mut cfg);
        assert_eq!(
            serde_json::to_value(&cfg.probe).unwrap()["targets"],
            serde_json::json!([
                { "address": "1.1.1.1", "method": "tcp", "port": 53 },
                { "address": "8.8.8.8", "method": "tcp", "port": 53 }
            ])
        );
        let mut off = probing(crate::ProbeConfig::default());
        store_probe_defaults(&mut off);
        assert!(off.probe.unwrap().targets.is_empty());
    }
}
