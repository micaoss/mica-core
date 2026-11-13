use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::IfaceSettings;

const A: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";
const B: &str = "HIgo9xNzJMWLKASShiTqIybxZ0U3wGLiUeJ1PKf8ykw=";

fn map(value: serde_json::Value) -> BTreeMap<String, IfaceSettings> {
    serde_json::from_value(value).expect("a network map")
}

fn tunnel(address: &str, peers: serde_json::Value) -> serde_json::Value {
    json!({
        "kind": "wireguard",
        "dhcp": false,
        "static": { "address": address },
        "wireguard": { "peers": peers },
    })
}

fn codes(
    previous: &BTreeMap<String, IfaceSettings>,
    candidate: &BTreeMap<String, IfaceSettings>,
) -> Vec<&'static str> {
    wireguard_route_violations(previous, candidate)
        .into_iter()
        .map(|violation| violation.code)
        .collect()
}

fn destinations(cfg: &IfaceSettings) -> Vec<(String, Option<String>)> {
    wireguard_routes(cfg)
        .into_iter()
        .map(|route| (route.destination, route.preferred_source))
        .collect()
}

/// WG-T01: a peer subnet beyond the tunnel's own address is a route out of
/// the tunnel, from the tunnel's address; a host prefix is normalized.
#[test]
fn a_peer_subnet_is_a_route_from_the_tunnel_address() {
    let network = map(json!({
        "wg102": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["10.202.0.9/24", "192.168.50.7"] },
        ])),
    }));
    assert_eq!(
        destinations(&network["wg102"]),
        [
            (
                "10.202.0.0/24".to_string(),
                Some("10.202.0.102".to_string())
            ),
            (
                "192.168.50.7/32".to_string(),
                Some("10.202.0.102".to_string())
            ),
        ]
    );
}

/// WG-T03/T04/T06: one route per distinct prefix, in a fixed order; a prefix
/// the tunnel's own subnet already reaches, a default route, a family the
/// tunnel has no address in and a prefix the entry declares as its own route
/// add none.
#[test]
fn routes_are_distinct_sorted_and_only_what_the_tunnel_needs() {
    let mut entry = tunnel(
        "10.202.0.102/24",
        json!([
            { "publicKey": B, "allowedIps": ["172.16.0.0/16", "10.202.0.0/24", "0.0.0.0/0"] },
            { "publicKey": A, "allowedIps": ["172.16.0.0/16", "10.9.0.0/16", "fd00::/64"] },
        ]),
    );
    entry["routes"] = json!([{ "destination": "10.9.0.0/16" }]);
    let network = map(json!({ "wg0": entry }));
    let routes = destinations(&network["wg0"]);
    assert_eq!(
        routes,
        [(
            "172.16.0.0/16".to_string(),
            Some("10.202.0.102".to_string())
        )]
    );
    assert_eq!(
        destinations(&network["wg0"]),
        routes,
        "the same input renders the same routes"
    );
}

/// A tunnel with no address carries no route of a family it cannot source.
#[test]
fn a_tunnel_without_an_address_has_no_routes() {
    let network = map(json!({
        "wg0": { "kind": "wireguard", "dhcp": false,
                 "wireguard": { "peers": [{ "publicKey": A, "allowedIps": ["10.1.0.0/16"] }] } },
    }));
    assert!(destinations(&network["wg0"]).is_empty());
}

/// WG-T10: a new default route through a peer is refused, a stored one is
/// kept and may be edited around and deleted.
#[test]
fn a_new_default_route_is_refused_and_a_stored_one_is_kept() {
    let empty = map(json!({ "wg0": tunnel("10.202.0.102/32", json!([])) }));
    for default in ["0.0.0.0/0", "::/0"] {
        let candidate = map(json!({
            "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": [default] }])),
        }));
        assert_eq!(
            codes(&empty, &candidate),
            ["wireguard_default_route_unsupported"],
            "{default}"
        );
    }

    let stored = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": ["0.0.0.0/0"], "name": "old" }])),
    }));
    let renamed = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": ["0.0.0.0/0"], "name": "new" }])),
    }));
    assert!(
        codes(&stored, &renamed).is_empty(),
        "an untouched stored /0 is not refused"
    );
    let added = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["0.0.0.0/0"] },
            { "publicKey": B, "allowedIps": ["10.5.0.0/16"] },
        ])),
    }));
    assert!(
        codes(&stored, &added).is_empty(),
        "another peer may be added beside it"
    );
    assert!(codes(&stored, &empty).is_empty(), "it may be deleted");
}

/// Prefixes no tunnel can carry are refused.
#[test]
fn reserved_prefixes_are_refused() {
    let empty = BTreeMap::new();
    for prefix in [
        "127.0.0.0/8",
        "224.0.0.0/4",
        "169.254.0.0/16",
        "255.255.255.255/32",
        "0.0.0.0/8",
    ] {
        let candidate = map(json!({
            "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": [prefix] }])),
        }));
        assert_eq!(
            codes(&empty, &candidate),
            ["wireguard_prefix_invalid"],
            "{prefix}"
        );
    }
}

/// WG-T13: the tunnel's address model is IPv4, so an IPv6 prefix has no
/// source and is refused; so is any prefix of a tunnel with no address.
#[test]
fn a_prefix_of_a_family_the_tunnel_has_no_address_in_is_refused() {
    let empty = BTreeMap::new();
    let candidate = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": ["fd00:1::/64"] }])),
    }));
    assert_eq!(codes(&empty, &candidate), ["wireguard_address_missing"]);
    let unaddressed = map(json!({
        "wg0": { "kind": "wireguard", "dhcp": false,
                 "wireguard": { "peers": [{ "publicKey": A, "allowedIps": ["10.1.0.0/16"] }] } },
    }));
    assert_eq!(codes(&empty, &unaddressed), ["wireguard_address_missing"]);
}

/// WG-T11: one prefix on two peers of a tunnel is refused, a longer one is
/// not; a prefix overlapping another tunnel's, falling inside another
/// entry's subnet or equal to another entry's route is refused.
#[test]
fn overlapping_prefixes_follow_the_conflict_rules() {
    let empty = BTreeMap::new();
    let same = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["10.202.0.0/24"] },
            { "publicKey": B, "allowedIps": ["10.202.0.0/24"] },
        ])),
    }));
    assert!(codes(&empty, &same).contains(&"wireguard_peer_prefix_conflict"));
    let longer = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["10.202.0.0/16"] },
            { "publicKey": B, "allowedIps": ["10.202.7.0/24"] },
        ])),
    }));
    assert!(codes(&empty, &longer).is_empty());

    let tunnels = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": ["10.50.0.0/16"] }])),
        "wg1": tunnel("10.203.0.102/32", json!([{ "publicKey": B, "allowedIps": ["10.50.3.0/24"] }])),
    }));
    let before = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": ["10.50.0.0/16"] }])),
        "wg1": tunnel("10.203.0.102/32", json!([])),
    }));
    assert_eq!(codes(&before, &tunnels), ["wireguard_route_conflict"]);

    let lan = json!({ "dhcp": false, "static": { "address": "192.168.1.10/24" },
                      "routes": [{ "destination": "172.20.0.0/16", "gateway": "192.168.1.1" }] });
    for (prefix, refused) in [
        ("192.168.1.128/25", true),
        ("172.20.0.0/16", true),
        ("192.168.0.0/16", false),
    ] {
        let candidate = map(json!({
            "eth0": lan,
            "wg0": tunnel("10.202.0.102/32", json!([{ "publicKey": A, "allowedIps": [prefix] }])),
        }));
        let found = codes(&empty, &candidate);
        assert_eq!(
            found.contains(&"wireguard_route_conflict"),
            refused,
            "{prefix}: {found:?}"
        );
    }
}

/// An endpoint a peer's own prefixes would carry is a loop; a host name is
/// not resolved here.
#[test]
fn an_endpoint_inside_the_routed_prefixes_is_a_loop() {
    let empty = BTreeMap::new();
    let candidate = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["203.0.113.0/24"], "endpoint": "203.0.113.10:51820" },
        ])),
    }));
    assert_eq!(codes(&empty, &candidate), ["wireguard_endpoint_route_loop"]);
    let other_peer = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["10.202.0.0/24"], "endpoint": "198.51.100.4:51820" },
            { "publicKey": B, "allowedIps": ["198.51.100.0/24"] },
        ])),
    }));
    assert_eq!(
        codes(&empty, &other_peer),
        ["wireguard_endpoint_route_loop"]
    );
    let named = map(json!({
        "wg0": tunnel("10.202.0.102/32", json!([
            { "publicKey": A, "allowedIps": ["10.202.0.0/24"], "endpoint": "vpn.example.net:51820" },
        ])),
    }));
    assert!(codes(&empty, &named).is_empty());
}

fn peer(key: &str, endpoint: Option<&str>, allowed: &[&str]) -> WireguardPeer {
    serde_json::from_value(json!({
        "publicKey": key,
        "endpoint": endpoint,
        "allowedIps": allowed,
    }))
    .expect("a peer")
}

/// A configured endpoint the kernel does not hold yet is pending; one the
/// kernel holds inside a prefix of any of the tunnel's peers is a loop; a
/// peer without an endpoint is ready.
#[test]
fn a_peer_state_reads_the_kernel_endpoint_against_the_tunnel_prefixes() {
    let a = peer(A, Some("vpn.example.net:51820"), &["10.8.0.0/24"]);
    let b = peer(B, None, &["192.0.2.0/24", "fd00:8::/64"]);
    let peers = [a.clone(), b.clone()];

    assert_eq!(
        wireguard_peer_state(&peers, &a, None),
        "pending_endpoint_resolution"
    );
    assert_eq!(wireguard_peer_state(&peers, &b, None), "ready");
    assert_eq!(
        wireguard_peer_state(&peers, &a, Some("198.51.100.4:51820")),
        "ready"
    );
    assert_eq!(
        wireguard_peer_state(&peers, &a, Some("192.0.2.9:51820")),
        "endpoint_route_loop"
    );
    assert_eq!(
        wireguard_peer_state(&peers, &a, Some("[fd00:8::3]:51820")),
        "endpoint_route_loop"
    );
}

/// A prefix counts as routed only when the kernel holds the same prefix and
/// sends it out of the tunnel; spellings of one prefix match.
#[test]
fn a_prefix_is_unrouted_unless_the_kernel_sends_it_out_of_the_tunnel() {
    let p = peer(
        A,
        None,
        &[
            "10.8.0.0/24",
            "10.9.0.1/16",
            "fd00:8:0::/64",
            "172.16.0.0/12",
        ],
    );
    let routed = vec![
        ("10.8.0.0/24".to_string(), Some(true)),
        ("10.9.0.0/16".to_string(), Some(true)),
        ("fd00:8::/64".to_string(), Some(false)),
    ];
    assert_eq!(
        wireguard_unrouted(&p, &routed),
        ["fd00:8:0::/64", "172.16.0.0/12"]
    );
    assert!(wireguard_unrouted(&peer(A, None, &["10.8.0.0/24"]), &routed).is_empty());
}
