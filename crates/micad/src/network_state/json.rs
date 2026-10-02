//! The observed network as the live-state tree reports it.

use serde_json::{Map, Value, json};

use super::*;

/// Format an address family and raw bytes the way `networkctl` prints them.
///
/// The family is optional because the byte length alone names it when the
/// source omits the family (networkd's `HardwareAddress` has none).
#[must_use]
pub fn format_address(family: Option<i64>, bytes: &[u8]) -> Option<String> {
    match (family, bytes.len()) {
        (Some(2) | None, 4) => {
            Some(std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string())
        }
        (Some(10) | None, 16) => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(bytes);
            Some(std::net::Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

/// A byte array as JSON numbers → bytes.
pub(super) fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    value
        .as_array()?
        .iter()
        .map(|item| u8::try_from(item.as_u64()?).ok())
        .collect()
}

pub(super) fn family_name(family: Option<i64>, bytes_len: usize) -> &'static str {
    match (family, bytes_len) {
        (Some(2), _) | (None, 4) => "ipv4",
        (Some(10), _) | (None, 16) => "ipv6",
        _ => "unknown",
    }
}

/// What an rtnetlink id with no name is reported as.
pub(super) const UNNAMED_ID: &str = "unknown";

/// Insert an rtnetlink enum as a name plus the raw id it was resolved from.
///
/// No id means neither member: networkd not describing the id is absence, and
/// absence is not a name. `name` of `None` is an id nothing names.
pub(super) fn insert_named_id(
    root: &mut Map<String, Value>,
    key: &str,
    id: Option<u64>,
    name: Option<&str>,
) {
    let Some(id) = id else { return };
    root.insert(key.to_string(), json!(name.unwrap_or(UNNAMED_ID)));
    root.insert(format!("{key}Id"), json!(id));
}

/// A route protocol id as `linux/rtnetlink.h` names it — the same set
/// `/etc/iproute2/rt_protos` ships, plus `RTPROT_MROUTED`, which the kernel
/// names and that file happens to omit.
pub(super) fn route_protocol_name(protocol: u64) -> Option<&'static str> {
    Some(match protocol {
        0 => "unspec",
        1 => "redirect",
        2 => "kernel",
        3 => "boot",
        4 => "static",
        8 => "gated",
        9 => "ra",
        10 => "mrt",
        11 => "zebra",
        12 => "bird",
        13 => "dnrouted",
        14 => "xorp",
        15 => "ntk",
        16 => "dhcp",
        17 => "mrouted",
        18 => "keepalived",
        42 => "babel",
        99 => "openr",
        186 => "bgp",
        187 => "isis",
        188 => "ospf",
        189 => "rip",
        192 => "eigrp",
        _ => return None,
    })
}

/// An address or route scope as `linux/rtnetlink.h` names it. The five values
/// it defines are the whole named set; 1–199 and 201–252 are the range it
/// hands to userspace, so a scope from there has no name to find.
pub(super) fn route_scope_name(scope: u64) -> Option<&'static str> {
    Some(match scope {
        0 => "global",
        200 => "site",
        253 => "link",
        254 => "host",
        255 => "nowhere",
        _ => return None,
    })
}

/// A route's table name, from networkd's `TableString`.
pub(super) fn route_table_name(route: &Value) -> Option<&str> {
    let name = route.get("TableString").and_then(Value::as_str)?;
    name.parse::<u64>().is_err().then_some(name)
}

/// networkd's `HardwareAddress` (an array of octets) as colon-separated hex.
pub(super) fn hardware_address(link: &Value) -> Option<String> {
    let bytes = bytes_of(link.get("HardwareAddress")?)?;
    if bytes.is_empty() {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// An address the way `Addresses` and a lease spell it: `Family` + `Address`
/// bytes, or just bytes.
pub(super) fn address_of(entry: &Value, key: &str) -> Option<(String, &'static str)> {
    let bytes = bytes_of(entry.get(key)?)?;
    let family = entry.get("Family").and_then(Value::as_i64);
    let name = family_name(family, bytes.len());
    Some((format_address(family, &bytes)?, name))
}

pub(super) fn address_json(entry: &Value) -> Option<Value> {
    let (address, family) = address_of(entry, "Address")?;
    let mut root = Map::new();
    root.insert("family".to_string(), json!(family));
    root.insert("address".to_string(), json!(address));
    if let Some(prefix) = entry.get("PrefixLength").and_then(Value::as_u64) {
        root.insert("prefixLength".to_string(), json!(prefix));
    }
    let scope = entry.get("Scope").and_then(Value::as_u64);
    insert_named_id(&mut root, "scope", scope, scope.and_then(route_scope_name));
    if let Some(source) = entry.get("ConfigSource").and_then(Value::as_str) {
        root.insert("configSource".to_string(), json!(source));
    }
    Some(Value::Object(root))
}

/// Whether a route is a default route: destination prefix length zero.
pub(super) fn is_default_route(route: &Value) -> bool {
    match route.get("DestinationPrefixLength").and_then(Value::as_u64) {
        Some(length) => length == 0,
        None => route
            .get("Destination")
            .and_then(bytes_of)
            .is_some_and(|bytes| bytes.iter().all(|byte| *byte == 0)),
    }
}

pub(super) fn default_route_json(
    route: &Value,
    interface: Option<&str>,
    index: Option<u64>,
) -> Value {
    let family = route.get("Family").and_then(Value::as_i64);
    let gateway = route
        .get("Gateway")
        .and_then(bytes_of)
        .and_then(|bytes| format_address(family, &bytes));
    let mut root = Map::new();
    root.insert(
        "family".to_string(),
        json!(family_name(
            family,
            route
                .get("Gateway")
                .and_then(bytes_of)
                .map_or(0, |b| b.len())
        )),
    );
    root.insert("gateway".to_string(), json!(gateway));
    root.insert("interface".to_string(), json!(interface));
    root.insert("interfaceIndex".to_string(), json!(index));
    if let Some(metric) = route.get("Priority").and_then(Value::as_u64) {
        root.insert("metric".to_string(), json!(metric));
    }
    let protocol = route.get("Protocol").and_then(Value::as_u64);
    insert_named_id(
        &mut root,
        "protocol",
        protocol,
        protocol.and_then(route_protocol_name),
    );
    let table = route.get("Table").and_then(Value::as_u64);
    insert_named_id(&mut root, "table", table, route_table_name(route));
    if let Some(source) = route.get("ConfigSource").and_then(Value::as_str) {
        root.insert("configSource".to_string(), json!(source));
    }
    Value::Object(root)
}

pub(super) use micad_settings::absent;

/// The `dhcp` member of one interface: the DHCPv4 client's state and lease
/// when networkd reports a client, else — because the `Addresses` list says
/// where each address came from — an inferred lease from a DHCPv4-sourced
/// address, marked as inferred, else absent.
pub(super) fn dhcp_json(link: &Value) -> Value {
    if let Some(client) = link.get("DHCPv4Client").and_then(Value::as_object) {
        let mut root = Map::new();
        root.insert("available".to_string(), json!(true));
        root.insert("inferred".to_string(), json!(false));
        if let Some(state) = client.get("State").and_then(Value::as_str) {
            root.insert("state".to_string(), json!(state));
        }
        match client.get("Lease").and_then(Value::as_object) {
            Some(lease) => {
                let mut lease_json = Map::new();
                let lease_value = Value::Object(lease.clone());
                if let Some((address, _)) = address_of(&lease_value, "Address") {
                    lease_json.insert("address".to_string(), json!(address));
                }
                if let Some(prefix) = lease.get("PrefixLength").and_then(Value::as_u64) {
                    lease_json.insert("prefixLength".to_string(), json!(prefix));
                }
                if let Some((server, _)) = address_of(&lease_value, "ServerAddress") {
                    lease_json.insert("server".to_string(), json!(server));
                }
                if let Some(router) = lease
                    .get("Router")
                    .and_then(Value::as_array)
                    .and_then(|routers| routers.first())
                    .and_then(bytes_of)
                    .and_then(|bytes| format_address(None, &bytes))
                {
                    lease_json.insert("router".to_string(), json!(router));
                }
                if let Some(lifetime) = lease.get("LifetimeUSec").and_then(Value::as_u64) {
                    lease_json.insert("lifetimeSeconds".to_string(), json!(lifetime / 1_000_000));
                }
                root.insert("lease".to_string(), Value::Object(lease_json));
            }
            None => {
                root.insert(
                    "lease".to_string(),
                    absent("the DHCPv4 client holds no lease"),
                );
            }
        }
        return Value::Object(root);
    }
    let leased = link
        .get("Addresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|entry| entry.get("ConfigSource").and_then(Value::as_str) == Some("DHCPv4"));
    match leased {
        Some(entry) => {
            let mut lease = Map::new();
            if let Some((address, _)) = address_of(entry, "Address") {
                lease.insert("address".to_string(), json!(address));
            }
            if let Some(prefix) = entry.get("PrefixLength").and_then(Value::as_u64) {
                lease.insert("prefixLength".to_string(), json!(prefix));
            }
            if let Some(server) = entry
                .get("ConfigProvider")
                .and_then(bytes_of)
                .and_then(|bytes| format_address(None, &bytes))
            {
                lease.insert("server".to_string(), json!(server));
            }
            json!({
                "available": true,
                "inferred": true,
                "detail": "networkd reports no DHCPv4 client object; the lease is inferred from a DHCPv4-sourced address",
                "lease": Value::Object(lease),
            })
        }
        None => absent("no DHCPv4 client and no DHCPv4-sourced address on this interface"),
    }
}

pub(super) fn wifi_json(association: &WifiAssociation) -> Value {
    let mut root = Map::new();
    root.insert("interface".to_string(), json!(association.interface));
    match &association.detail {
        Some(detail) if association.state.is_none() => {
            root.insert("available".to_string(), json!(false));
            root.insert("detail".to_string(), json!(detail));
            return Value::Object(root);
        }
        _ => {}
    }
    root.insert("available".to_string(), json!(true));
    root.insert("state".to_string(), json!(association.state));
    root.insert(
        "associated".to_string(),
        json!(association.state.as_deref() == Some("COMPLETED")),
    );
    if let Some(ssid) = &association.ssid {
        root.insert("ssid".to_string(), json!(ssid));
    }
    if let Some(bssid) = &association.bssid {
        root.insert("bssid".to_string(), json!(bssid));
    }
    if let Some(frequency) = association.frequency_mhz {
        root.insert("frequencyMhz".to_string(), json!(frequency));
    }
    if let Some(key_management) = &association.key_management {
        root.insert("keyManagement".to_string(), json!(key_management));
    }
    if let Some(rssi) = association.rssi_dbm {
        root.insert("rssiDbm".to_string(), json!(rssi));
    }
    if let Some(speed) = association.link_speed_mbps {
        root.insert("linkSpeedMbps".to_string(), json!(speed));
    }
    Value::Object(root)
}

pub(super) fn interface_json(link: &Value, wifi: Option<&WifiAssociation>) -> Value {
    let mut root = Map::new();
    for (from, to) in [
        ("Name", "name"),
        ("Index", "index"),
        ("Kind", "kind"),
        ("Type", "type"),
        ("Driver", "driver"),
        ("MTU", "mtu"),
    ] {
        if let Some(value) = link.get(from) {
            root.insert(to.to_string(), value.clone());
        }
    }
    let carrier = link.get("CarrierState").and_then(Value::as_str);
    root.insert(
        "link".to_string(),
        json!({
            "administrativeState": link.get("AdministrativeState"),
            "operationalState": link.get("OperationalState"),
            "carrierState": carrier,
            "carrier": carrier.map(|state| state == "carrier" || state == "enslaved"),
            "onlineState": link.get("OnlineState"),
            "addressState": link.get("AddressState"),
        }),
    );
    if let Some(address) = hardware_address(link) {
        root.insert("hardwareAddress".to_string(), json!(address));
    }
    let addresses: Vec<Value> = link
        .get("Addresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(address_json)
        .collect();
    root.insert("addresses".to_string(), Value::Array(addresses));
    root.insert("dhcp".to_string(), dhcp_json(link));
    let dns: Vec<Value> = link
        .get("DNS")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| address_of(entry, "Address").map(|(address, _)| json!(address)))
        .collect();
    root.insert("dns".to_string(), Value::Array(dns));
    if let Some(association) = wifi {
        root.insert("wifi".to_string(), wifi_json(association));
    }
    Value::Object(root)
}

/// The `dns` member: the per-link servers networkd reports, the resolver's
/// own list and the probe.
pub(super) fn dns_json(evidence: Option<&DnsEvidence>, link_servers: Vec<String>) -> Value {
    let mut root = Map::new();
    root.insert("linkServers".to_string(), json!(link_servers));
    match evidence {
        None => {
            root.insert("available".to_string(), json!(false));
            root.insert(
                "detail".to_string(),
                json!("the resolver was not asked: this observer runs no DNS probe"),
            );
        }
        Some(evidence) => {
            root.insert("available".to_string(), json!(evidence.resolver_reachable));
            if !evidence.resolver_reachable {
                root.insert(
                    "detail".to_string(),
                    json!("systemd-resolved is not reachable on the bus"),
                );
            }
            root.insert("resolverServers".to_string(), json!(evidence.servers));
            root.insert(
                "probe".to_string(),
                match &evidence.probe {
                    None => absent("no probe ran"),
                    Some((name, outcome)) => {
                        let (reachable, result, detail) = match outcome {
                            DnsOutcome::Resolved { addresses } => {
                                (true, "resolved", format!("{addresses} address(es)"))
                            }
                            DnsOutcome::Failed(detail) => (false, "failed", detail.clone()),
                            DnsOutcome::TimedOut => (
                                false,
                                "timeout",
                                format!("no answer within {DNS_PROBE_TIMEOUT:?}"),
                            ),
                        };
                        json!({
                            "available": true,
                            "name": name,
                            "reachable": reachable,
                            "result": result,
                            "detail": detail,
                        })
                    }
                },
            );
        }
    }
    Value::Object(root)
}

/// The observed network state, rendered.
///
/// `describe` is networkd's raw `Describe` or the reason it is absent;
/// `wifi`, `dns` and `radios` are what the other observers produced. Every
/// member carries `available`; an unavailable one carries the reason.
#[must_use]
pub fn observed_json(
    describe: Result<&Value, &str>,
    wifi: &WifiEvidence,
    dns: Option<&DnsEvidence>,
    radios: &RadioEvidence,
) -> Value {
    let links: Vec<&Value> = describe
        .ok()
        .and_then(|value| value.get("Interfaces"))
        .and_then(Value::as_array)
        .map(|interfaces| interfaces.iter().collect())
        .unwrap_or_default();
    let association_for = |name: Option<&str>| {
        name.and_then(|name| {
            wifi.associations
                .iter()
                .find(|association| association.interface == name)
        })
    };
    let interfaces = if let Err(detail) = describe {
        absent(format!("systemd-networkd did not answer: {detail}"))
    } else {
        let entries: Vec<Value> = links
            .iter()
            .map(|link| {
                interface_json(
                    link,
                    association_for(link.get("Name").and_then(Value::as_str)),
                )
            })
            .collect();
        json!({ "available": true, "count": entries.len(), "entries": entries })
    };
    let default_routes = if let Err(detail) = describe {
        absent(format!("systemd-networkd did not answer: {detail}"))
    } else {
        let entries: Vec<Value> = links
            .iter()
            .flat_map(|link| {
                let name = link.get("Name").and_then(Value::as_str);
                let index = link.get("Index").and_then(Value::as_u64);
                link.get("Routes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|route| is_default_route(route))
                    .map(move |route| default_route_json(route, name, index))
            })
            .collect();
        json!({ "available": true, "count": entries.len(), "entries": entries })
    };
    let link_servers: Vec<String> = links
        .iter()
        .flat_map(|link| {
            link.get("DNS")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| address_of(entry, "Address").map(|(address, _)| address))
        })
        .collect();
    let wifi_member = if radios.wifi_interfaces.is_empty() {
        absent("no wireless interface on this board")
    } else if !wifi.control_dir_present {
        json!({
            "available": false,
            "detail": "wpa_supplicant's control directory is absent: no station is running",
            "interfaces": radios.wifi_interfaces,
        })
    } else {
        json!({
            "available": true,
            "associations": wifi.associations.iter().map(wifi_json).collect::<Vec<_>>(),
        })
    };
    // Named apart from `wifi`: an association is this device joining someone
    // else's network, and a station is someone else joining this device's.
    let access_points = if wifi.access_points.is_empty() {
        absent("hostapd is not running on any interface of this device")
    } else {
        json!({
            "available": true,
            "entries": wifi
                .access_points
                .iter()
                .map(access_point_json)
                .collect::<Vec<_>>(),
        })
    };
    json!({
        "interfaces": interfaces,
        "defaultRoutes": default_routes,
        "dns": dns_json(dns, link_servers),
        "wifi": wifi_member,
        "accessPoint": access_points,
        "capabilities": {
            "wifi": {
                "supported": !radios.wifi_interfaces.is_empty(),
                "interfaces": radios.wifi_interfaces,
                "detail": if radios.wifi_interfaces.is_empty() {
                    "no phy80211 device under /sys/class/net"
                } else {
                    "station and access point are driven by the wifi reconcilers"
                },
            },
            "bluetooth": {
                "supported": !radios.bluetooth_adapters.is_empty(),
                "adapters": radios.bluetooth_adapters,
                "detail": "adapter presence only; no Bluetooth state is observed by this surface",
            },
            "cellular": {
                "supported": false,
                "interfaces": radios.wwan_interfaces,
                "detail": "no cellular modem support in this image: SKU-specific and not selected",
            },
        },
    })
}
