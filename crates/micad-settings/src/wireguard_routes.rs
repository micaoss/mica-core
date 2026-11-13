//! The routes a WireGuard tunnel's peers ask for, and the rules a write of
//! them is held to.
//!
//! A peer's `allowedIps` are rendered as the tunnel's own routes, so the
//! routes are networkd's configuration and networkd adds, keeps and removes
//! them; nothing here installs one. The write rules apply to the peers a
//! write changes (a new peer, or new prefixes or endpoint): what is already
//! stored keeps running as it is, a default route through a peer included.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use crate::{IfaceKind, IfaceSettings, NetworkViolation, WireguardPeer};

/// One route a tunnel's peers ask for: a destination out of the tunnel, from
/// the tunnel's own address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireguardRoute {
    pub destination: String,
    pub preferred_source: Option<String>,
}

/// A network prefix, held as its network address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Prefix {
    addr: IpAddr,
    len: u8,
}

fn width(addr: IpAddr) -> u8 {
    if addr.is_ipv4() { 32 } else { 128 }
}

fn masked(addr: IpAddr, len: u8) -> IpAddr {
    match addr {
        IpAddr::V4(v4) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
            IpAddr::V4((u32::from(v4) & mask).into())
        }
        IpAddr::V6(v6) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
            IpAddr::V6((u128::from(v6) & mask).into())
        }
    }
}

impl Prefix {
    /// `a.b.c.d/len`, `x::y/len`, or a bare address as a host prefix.
    fn parse(text: &str) -> Option<Self> {
        let (addr, len) = match text.split_once('/') {
            Some((addr, len)) => (addr.parse::<IpAddr>().ok()?, len.parse::<u8>().ok()?),
            None => {
                let addr = text.parse::<IpAddr>().ok()?;
                (addr, width(addr))
            }
        };
        (len <= width(addr)).then(|| Self {
            addr: masked(addr, len),
            len,
        })
    }

    fn contains(&self, other: &Self) -> bool {
        self.addr.is_ipv4() == other.addr.is_ipv4()
            && self.len <= other.len
            && masked(other.addr, self.len) == self.addr
    }

    fn contains_addr(&self, addr: IpAddr) -> bool {
        self.contains(&Self {
            addr,
            len: width(addr),
        })
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.contains(other) || other.contains(self)
    }

    /// A range no tunnel carries: loopback, multicast, link-local,
    /// unspecified, broadcast.
    fn reserved(&self) -> bool {
        match self.addr {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_multicast()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.octets()[0] == 0
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_multicast()
                    || v6.is_unspecified()
                    || v6.segments()[0] & 0xffc0 == 0xfe80
            }
        }
    }
}

impl std::fmt::Display for Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.len)
    }
}

/// The tunnel's own address and the subnet it reaches on-link.
fn own_address(cfg: &IfaceSettings) -> Option<(IpAddr, Prefix)> {
    if cfg.dhcp {
        return None;
    }
    let address = &cfg.static_.as_ref()?.address;
    let host = address
        .split_once('/')
        .map_or(address.as_str(), |(host, _)| host);
    Some((host.parse().ok()?, Prefix::parse(address)?))
}

/// The numeric address of a `host:port` endpoint; a host name has none here.
fn endpoint_addr(endpoint: &str) -> Option<IpAddr> {
    let (host, _) = endpoint.rsplit_once(':')?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

fn peer_prefixes(peer: &WireguardPeer) -> impl Iterator<Item = Prefix> + '_ {
    peer.allowed_ips
        .iter()
        .filter_map(|allowed| Prefix::parse(allowed))
}

/// The routes a tunnel's peers ask for beyond what the tunnel already has:
/// one per distinct prefix, sorted, and none for a default route, for a
/// prefix of a family the tunnel has no address in, for a prefix its own
/// subnet reaches on-link, or for one the entry declares as its own route.
pub fn wireguard_routes(cfg: &IfaceSettings) -> Vec<WireguardRoute> {
    let (Some(wireguard), Some((source, subnet))) = (&cfg.wireguard, own_address(cfg)) else {
        return Vec::new();
    };
    let declared: BTreeSet<Prefix> = cfg
        .routes
        .iter()
        .filter_map(|route| Prefix::parse(&route.destination))
        .collect();
    let wanted: BTreeSet<Prefix> = wireguard
        .peers
        .iter()
        .flat_map(peer_prefixes)
        .filter(|prefix| {
            prefix.len > 0
                && prefix.addr.is_ipv4() == source.is_ipv4()
                && !subnet.contains(prefix)
                && !declared.contains(prefix)
        })
        .collect();
    wanted
        .into_iter()
        .map(|prefix| WireguardRoute {
            destination: prefix.to_string(),
            preferred_source: Some(source.to_string()),
        })
        .collect()
}

/// Whether `peer` is one this write changes, against the tunnel as stored:
/// new, or with other prefixes or another endpoint, or on a tunnel whose
/// address changed. A new name, keepalive or pre-shared key is not.
fn changed(
    peer: &WireguardPeer,
    iface: &str,
    cfg: &IfaceSettings,
    previous: &BTreeMap<String, IfaceSettings>,
) -> bool {
    let Some(stored) = previous.get(iface) else {
        return true;
    };
    if own_address(stored).map(|(addr, prefix)| (addr, prefix.len))
        != own_address(cfg).map(|(addr, prefix)| (addr, prefix.len))
    {
        return true;
    }
    !stored
        .wireguard
        .iter()
        .flat_map(|wireguard| &wireguard.peers)
        .any(|old| {
            old.public_key == peer.public_key
                && old.allowed_ips == peer.allowed_ips
                && old.endpoint == peer.endpoint
        })
}

/// The route rules a write of `candidate` breaks, for the peers it changes
/// against `previous`:
///
/// - `wireguard_default_route_unsupported`: a default route through a peer;
/// - `wireguard_prefix_invalid`: loopback, multicast, link-local,
///   unspecified or broadcast;
/// - `wireguard_address_missing`: the tunnel has no address of the prefix's
///   family to send from;
/// - `wireguard_peer_prefix_conflict`: the same prefix on another peer of
///   the tunnel (a longer one is allowed);
/// - `wireguard_route_conflict`: overlapping another tunnel's prefix, inside
///   another entry's subnet, or equal to another entry's route;
/// - `wireguard_endpoint_route_loop`: a numeric endpoint a tunnel prefix
///   would carry. A host name is resolved by networkd, not here.
pub fn wireguard_route_violations(
    previous: &BTreeMap<String, IfaceSettings>,
    candidate: &BTreeMap<String, IfaceSettings>,
) -> Vec<NetworkViolation> {
    let tunnels: Vec<(&str, &IfaceSettings, &[WireguardPeer])> = candidate
        .iter()
        .filter(|(_, cfg)| cfg.kind == IfaceKind::Wireguard)
        .filter_map(|(iface, cfg)| {
            Some((
                iface.as_str(),
                cfg,
                cfg.wireguard.as_ref()?.peers.as_slice(),
            ))
        })
        .collect();
    // Every routed prefix of every tunnel, by tunnel and peer key.
    let routed: Vec<(&str, &str, Prefix)> = tunnels
        .iter()
        .flat_map(|(iface, _, peers)| {
            peers.iter().flat_map(move |peer| {
                peer_prefixes(peer)
                    .filter(|prefix| prefix.len > 0)
                    .map(move |prefix| (*iface, peer.public_key.as_str(), prefix))
            })
        })
        .collect();
    let mut found = Vec::new();
    for (iface, cfg, peers) in &tunnels {
        let own = own_address(cfg);
        for (index, peer) in peers.iter().enumerate() {
            if !changed(peer, iface, cfg, previous) {
                continue;
            }
            let mut push = |code: &'static str, message: String| {
                found.push(NetworkViolation::new(
                    iface,
                    "wireguard.peers",
                    code,
                    serde_json::json!({ "peer": index }),
                    format!("network.{iface} peer {index}: {message}"),
                ));
            };
            for prefix in peer_prefixes(peer) {
                if prefix.len == 0 {
                    push(
                        "wireguard_default_route_unsupported",
                        "a default route through a WireGuard peer is not supported; name the networks behind it".into(),
                    );
                    continue;
                }
                if prefix.reserved() {
                    push(
                        "wireguard_prefix_invalid",
                        format!(
                            "{prefix} is a loopback, multicast, link-local, unspecified or broadcast range"
                        ),
                    );
                    continue;
                }
                if !own.is_some_and(|(addr, _)| addr.is_ipv4() == prefix.addr.is_ipv4()) {
                    push(
                        "wireguard_address_missing",
                        format!("the tunnel has no address of the family of {prefix} to send from"),
                    );
                    continue;
                }
                if routed.iter().any(|(other_iface, key, other)| {
                    other_iface == iface && *key != peer.public_key && *other == prefix
                }) {
                    push(
                        "wireguard_peer_prefix_conflict",
                        format!("{prefix} is another peer's of this tunnel"),
                    );
                }
                if let Some((other, _, _)) = routed
                    .iter()
                    .find(|(other_iface, _, other)| other_iface != iface && other.overlaps(&prefix))
                {
                    push(
                        "wireguard_route_conflict",
                        format!("{prefix} overlaps a prefix of network.{other}"),
                    );
                }
                for (other, entry) in candidate.iter().filter(|(other, _)| other != iface) {
                    let inside =
                        own_address(entry).is_some_and(|(_, subnet)| subnet.contains(&prefix));
                    let declared = entry
                        .routes
                        .iter()
                        .any(|route| Prefix::parse(&route.destination) == Some(prefix));
                    if inside || declared {
                        push(
                            "wireguard_route_conflict",
                            format!("{prefix} is network.{other}'s subnet or route"),
                        );
                    }
                }
                // A stored peer's endpoint the new prefix would carry; a
                // changed peer's endpoint is checked below, once.
                let looped = tunnels
                    .iter()
                    .flat_map(|(other_iface, other_cfg, other_peers)| {
                        other_peers
                            .iter()
                            .filter(move |other| !changed(other, other_iface, other_cfg, previous))
                    })
                    .filter_map(|other| endpoint_addr(other.endpoint.as_deref()?))
                    .any(|addr| prefix.contains_addr(addr));
                if looped {
                    push(
                        "wireguard_endpoint_route_loop",
                        format!("{prefix} would carry a peer's endpoint into a tunnel"),
                    );
                }
            }
            if let Some(addr) = peer.endpoint.as_deref().and_then(endpoint_addr)
                && routed
                    .iter()
                    .any(|(_, _, prefix)| prefix.contains_addr(addr))
            {
                push(
                    "wireguard_endpoint_route_loop",
                    format!("the endpoint {addr} is inside a prefix routed into a tunnel"),
                );
            }
        }
    }
    found
}

/// Where a peer stands, read from what the kernel reports of it.
///
/// - `pending_endpoint_resolution`: an endpoint is configured and the kernel
///   holds none yet (a host name networkd has not resolved);
/// - `endpoint_route_loop`: the kernel's endpoint lies inside a prefix one of
///   the tunnel's peers routes into the tunnel, so the tunnel's own packets
///   would be sent through itself;
/// - `ready` otherwise, a peer with no endpoint that waits to be reached
///   included.
#[must_use]
pub fn wireguard_peer_state(
    peers: &[WireguardPeer],
    peer: &WireguardPeer,
    kernel_endpoint: Option<&str>,
) -> &'static str {
    let Some(endpoint) = kernel_endpoint else {
        return if peer.endpoint.is_some() {
            "pending_endpoint_resolution"
        } else {
            "ready"
        };
    };
    let looped = endpoint_addr(endpoint).is_some_and(|addr| {
        peers
            .iter()
            .flat_map(peer_prefixes)
            .any(|prefix| prefix.contains_addr(addr))
    });
    if looped {
        "endpoint_route_loop"
    } else {
        "ready"
    }
}

/// The peer's allowed IPs the kernel does not send out of the tunnel: those
/// whose `routed` entry (a kernel prefix and whether its network address
/// leaves by the tunnel) is absent or not `true`, in stored order.
#[must_use]
pub fn wireguard_unrouted(peer: &WireguardPeer, routed: &[(String, Option<bool>)]) -> Vec<String> {
    let routed: Vec<(Prefix, bool)> = routed
        .iter()
        .filter_map(|(prefix, ok)| Some((Prefix::parse(prefix)?, *ok == Some(true))))
        .collect();
    peer.allowed_ips
        .iter()
        .filter(|allowed| {
            Prefix::parse(allowed)
                .is_none_or(|wanted| !routed.iter().any(|(seen, ok)| *seen == wanted && *ok))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
