//! The writes that run a WireGuard tunnel on a root without networkd: the
//! link, its private key and peers (generic netlink `WG_CMD_SET_DEVICE`), its
//! address, and the routes micad owns on it.
//!
//! Each request asks for an acknowledgement, so a refusal comes back as the
//! error the kernel names. The builders return the request bytes, so they are
//! tested without a kernel.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use rustix::net::netlink;

use super::{
    AF_INET, AF_INET6, IFA_LOCAL, IFADDRMSG, IFINFOMSG, IFLA_IFNAME, IFLA_INFO_KIND, IFLA_LINKINFO,
    NLM_F_DUMP, RTA_DST, RTA_GATEWAY, RTA_OIF, RTM_GETADDR, RTM_GETLINK, RTM_GETROUTE, RTM_NEWADDR,
    RTM_NEWROUTE, RTMSG, WG_GENL_VERSION, attribute, attributes, exchange, message, name_payload,
    u32_at, wireguard_family,
};

/// The alias micad gives every link it creates: what makes a WireGuard link
/// micad's to delete. A container on the host network may run its own.
pub const LINK_ALIAS: &str = "mica";

/// The `rtm_protocol` of every route micad installs on a tunnel: what makes a
/// route micad's to delete. Free in the kernel's and iproute2's lists.
pub const RTPROT_MICA: u8 = 109;

const RTM_NEWLINK: u16 = 16;
const RTM_DELLINK: u16 = 17;
const RTM_DELADDR: u16 = 21;
const RTM_DELROUTE: u16 = 25;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_REPLACE: u16 = 0x100;
const NLM_F_EXCL: u16 = 0x200;
const NLM_F_CREATE: u16 = 0x400;
/// WireGuard parses its nests strictly: each one carries the flag.
const NLA_F_NESTED: u16 = 0x8000;
const IFF_UP: u32 = 0x1;
const IFLA_IFALIAS: u16 = 20;
const IFA_ADDRESS: u16 = 1;
const RTA_PRIORITY: u16 = 6;
const RTA_PREFSRC: u16 = 7;
const RTA_TABLE: u16 = 15;
const RT_TABLE_MAIN: u8 = 254;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RTN_UNICAST: u8 = 1;
const WG_KIND: &str = "wireguard";
const WG_CMD_SET_DEVICE: u8 = 1;
const WGDEVICE_A_IFNAME: u16 = 2;
const WGDEVICE_A_PRIVATE_KEY: u16 = 3;
const WGDEVICE_A_LISTEN_PORT: u16 = 6;
const WGDEVICE_A_PEERS: u16 = 8;
const WGPEER_A_PUBLIC_KEY: u16 = 1;
const WGPEER_A_PRESHARED_KEY: u16 = 2;
const WGPEER_A_FLAGS: u16 = 3;
const WGPEER_A_ENDPOINT: u16 = 4;
const WGPEER_A_PERSISTENT_KEEPALIVE_INTERVAL: u16 = 5;
const WGPEER_A_ALLOWEDIPS: u16 = 9;
const WGALLOWEDIP_A_FAMILY: u16 = 1;
const WGALLOWEDIP_A_IPADDR: u16 = 2;
const WGALLOWEDIP_A_CIDR_MASK: u16 = 3;
const WGPEER_F_REMOVE_ME: u32 = 0x1;
const WGPEER_F_REPLACE_ALLOWEDIPS: u32 = 0x2;
const WGPEER_F_UPDATE_ONLY: u32 = 0x4;

/// An address with its prefix length.
pub type Prefix = (IpAddr, u8);

/// One route out of a tunnel. `metric` is the one the kernel reports: an
/// IPv6 route asked for without one holds 1024.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TunnelRoute {
    pub destination: Prefix,
    pub gateway: Option<IpAddr>,
    pub source: Option<IpAddr>,
    pub metric: u32,
}

/// The metric the kernel gives a route of `destination`'s family asked for
/// with `metric`.
#[must_use]
pub fn kernel_metric(destination: IpAddr, metric: Option<u32>) -> u32 {
    match (metric, destination) {
        (Some(metric), IpAddr::V4(_)) => metric,
        (None, IpAddr::V4(_)) => 0,
        (Some(0) | None, IpAddr::V6(_)) => 1024,
        (Some(metric), IpAddr::V6(_)) => metric,
    }
}

/// One peer as a `WG_CMD_SET_DEVICE` sets it: whole, its allowed IPs
/// replacing the ones it had. An absent pre-shared key or keepalive clears
/// the one the kernel holds.
#[derive(Clone, PartialEq, Eq)]
pub struct PeerSet {
    pub public_key: [u8; 32],
    pub preshared_key: Option<[u8; 32]>,
    pub endpoint: Option<SocketAddr>,
    pub keepalive: Option<u16>,
    pub allowed_ips: Vec<Prefix>,
}

/// A `WG_CMD_SET_DEVICE`: the device's key and port, the peers it sets, and
/// the peers it removes. The peers it names neither are left as they are.
pub struct DeviceSet {
    pub private_key: [u8; 32],
    pub listen_port: Option<u16>,
    pub peers: Vec<PeerSet>,
    pub remove: Vec<[u8; 32]>,
}

/// The kernel side of a tunnel, as the OpenRC network reconciler drives it.
pub trait TunnelKernel: Send + Sync {
    /// Every WireGuard link micad created ([`LINK_ALIAS`]), its index by
    /// name.
    fn owned_links(&self) -> io::Result<BTreeMap<String, u32>>;
    /// Create the WireGuard link `name`, down and with [`LINK_ALIAS`], and
    /// return its index. A link of that name already there is a refusal.
    fn create(&self, name: &str) -> io::Result<u32>;
    /// Delete link `index`, and with it its addresses and routes.
    fn delete(&self, index: u32) -> io::Result<()>;
    /// Bring link `index` up.
    fn up(&self, index: u32) -> io::Result<()>;
    /// The global-scope addresses on link `index`.
    fn addresses(&self, index: u32) -> io::Result<Vec<Prefix>>;
    fn add_address(&self, index: u32, address: Prefix) -> io::Result<()>;
    fn delete_address(&self, index: u32, address: Prefix) -> io::Result<()>;
    /// The base64 public keys of the peers device `name` holds.
    fn peers(&self, name: &str) -> io::Result<Vec<String>>;
    fn set_device(&self, name: &str, device: &DeviceSet) -> io::Result<()>;
    /// Point device `name`'s peer `public_key` at `endpoint`, and change
    /// nothing else: a peer the device no longer holds stays gone.
    fn set_endpoint(
        &self,
        name: &str,
        public_key: &[u8; 32],
        endpoint: SocketAddr,
    ) -> io::Result<()>;
    /// The main-table routes out of link `index`, each with its protocol.
    fn routes(&self, index: u32) -> io::Result<Vec<(TunnelRoute, u8)>>;
    /// Add `route` out of link `index` as micad's ([`RTPROT_MICA`]).
    fn add_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()>;
    /// Delete micad's `route` out of link `index`.
    fn delete_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()>;
}

/// The running kernel.
pub struct Kernel;

impl TunnelKernel for Kernel {
    fn owned_links(&self) -> io::Result<BTreeMap<String, u32>> {
        let body = vec![0u8; IFINFOMSG];
        Ok(exchange(None, &message(RTM_GETLINK, NLM_F_DUMP, &body))?
            .iter()
            .filter_map(|(_, body)| owned_link(body))
            .collect())
    }

    fn create(&self, name: &str) -> io::Result<u32> {
        exchange(None, &new_link(name))?;
        let mut body = vec![0u8; IFINFOMSG];
        body.extend(attribute(IFLA_IFNAME, &name_payload(name)));
        let index = exchange(None, &message(RTM_GETLINK, 0, &body))?
            .iter()
            .find_map(|(_, body)| u32_at(body, 4))
            .ok_or_else(|| io::Error::other(format!("{name} was created and is not there")))?;
        exchange(None, &mark_link(index))?;
        Ok(index)
    }

    fn delete(&self, index: u32) -> io::Result<()> {
        exchange(None, &delete_link(index)).map(drop)
    }

    fn up(&self, index: u32) -> io::Result<()> {
        exchange(None, &link_up(index)).map(drop)
    }

    fn addresses(&self, index: u32) -> io::Result<Vec<Prefix>> {
        let body = vec![0u8; IFADDRMSG];
        Ok(exchange(None, &message(RTM_GETADDR, NLM_F_DUMP, &body))?
            .iter()
            .filter(|(kind, _)| *kind == RTM_NEWADDR)
            .filter_map(|(_, body)| link_address(body, index))
            .collect())
    }

    fn add_address(&self, index: u32, address: Prefix) -> io::Result<()> {
        exchange(None, &address_request(RTM_NEWADDR, index, address)).map(drop)
    }

    fn delete_address(&self, index: u32, address: Prefix) -> io::Result<()> {
        exchange(None, &address_request(RTM_DELADDR, index, address)).map(drop)
    }

    fn peers(&self, name: &str) -> io::Result<Vec<String>> {
        Ok(super::wireguard_device(name)?
            .into_iter()
            .map(|peer| peer.public_key)
            .collect())
    }

    fn set_device(&self, name: &str, device: &DeviceSet) -> io::Result<()> {
        let family = wireguard_family()?;
        exchange(Some(netlink::GENERIC), &set_device(family, name, device)).map(drop)
    }

    fn set_endpoint(
        &self,
        name: &str,
        public_key: &[u8; 32],
        endpoint: SocketAddr,
    ) -> io::Result<()> {
        let family = wireguard_family()?;
        exchange(
            Some(netlink::GENERIC),
            &set_endpoint(family, name, public_key, endpoint),
        )
        .map(drop)
    }

    fn routes(&self, index: u32) -> io::Result<Vec<(TunnelRoute, u8)>> {
        let body = vec![0u8; RTMSG];
        Ok(exchange(None, &message(RTM_GETROUTE, NLM_F_DUMP, &body))?
            .iter()
            .filter(|(kind, _)| *kind == RTM_NEWROUTE)
            .filter_map(|(_, body)| link_route(body, index))
            .collect())
    }

    fn add_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()> {
        exchange(None, &route_request(RTM_NEWROUTE, index, route)).map(drop)
    }

    fn delete_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()> {
        exchange(None, &route_request(RTM_DELROUTE, index, route)).map(drop)
    }
}

fn nested(kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    attribute(kind | NLA_F_NESTED, &children.concat())
}

fn family_of(address: IpAddr) -> u8 {
    if address.is_ipv4() { AF_INET } else { AF_INET6 }
}

fn octets(address: IpAddr) -> Vec<u8> {
    match address {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

fn address_of(family: u8, payload: &[u8]) -> Option<IpAddr> {
    match family {
        AF_INET => Some(IpAddr::V4(Ipv4Addr::from(
            <[u8; 4]>::try_from(payload).ok()?,
        ))),
        AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(
            <[u8; 16]>::try_from(payload).ok()?,
        ))),
        _ => None,
    }
}

/// `struct ifinfomsg` for link `index`, with `flags` changed under `change`.
fn ifinfomsg(index: u32, flags: u32, change: u32) -> Vec<u8> {
    let mut body = vec![0u8; 4];
    body.extend(index.to_ne_bytes());
    body.extend(flags.to_ne_bytes());
    body.extend(change.to_ne_bytes());
    body
}

/// `RTM_NEWLINK` creating WireGuard link `name`; an existing link of that
/// name is a refusal.
pub(super) fn new_link(name: &str) -> Vec<u8> {
    let mut body = ifinfomsg(0, 0, 0);
    body.extend(attribute(IFLA_IFNAME, &name_payload(name)));
    body.extend(nested(
        IFLA_LINKINFO,
        &[attribute(IFLA_INFO_KIND, WG_KIND.as_bytes())],
    ));
    message(RTM_NEWLINK, NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL, &body)
}

pub(super) fn delete_link(index: u32) -> Vec<u8> {
    message(RTM_DELLINK, NLM_F_ACK, &ifinfomsg(index, 0, 0))
}

/// `RTM_NEWLINK` giving link `index` [`LINK_ALIAS`]. Its own request: a
/// creating one does not set an alias.
pub(super) fn mark_link(index: u32) -> Vec<u8> {
    let mut body = ifinfomsg(index, 0, 0);
    body.extend(attribute(IFLA_IFALIAS, LINK_ALIAS.as_bytes()));
    message(RTM_NEWLINK, NLM_F_ACK, &body)
}

pub(super) fn link_up(index: u32) -> Vec<u8> {
    message(RTM_NEWLINK, NLM_F_ACK, &ifinfomsg(index, IFF_UP, IFF_UP))
}

/// The name and index in an `RTM_NEWLINK` body, when it is a WireGuard link
/// with [`LINK_ALIAS`].
pub(super) fn owned_link(body: &[u8]) -> Option<(String, u32)> {
    let index = u32_at(body, 4)?;
    let attrs = attributes(body.get(IFINFOMSG..)?);
    let kind = attrs
        .iter()
        .find(|(kind, _)| *kind == IFLA_LINKINFO)
        .and_then(|(_, info)| {
            attributes(info)
                .into_iter()
                .find(|(kind, _)| *kind == IFLA_INFO_KIND)
                .map(|(_, payload)| payload.split(|byte| *byte == 0).next().unwrap_or_default())
        })?;
    let alias = attrs
        .iter()
        .find(|(kind, _)| *kind == IFLA_IFALIAS)
        .map(|(_, payload)| payload.split(|byte| *byte == 0).next().unwrap_or_default());
    if kind != WG_KIND.as_bytes() || alias != Some(LINK_ALIAS.as_bytes()) {
        return None;
    }
    let name = attrs
        .iter()
        .find(|(kind, _)| *kind == IFLA_IFNAME)
        .and_then(|(_, payload)| std::str::from_utf8(payload).ok())?
        .trim_end_matches('\0');
    Some((name.to_string(), index))
}

/// `RTM_NEWADDR` or `RTM_DELADDR` of `address` on link `index`. A new one
/// replaces the same address held with other properties.
pub(super) fn address_request(kind: u16, index: u32, address: Prefix) -> Vec<u8> {
    let (ip, length) = address;
    let mut body = vec![family_of(ip), length, 0, RT_SCOPE_UNIVERSE];
    body.extend(index.to_ne_bytes());
    body.extend(attribute(IFA_LOCAL, &octets(ip)));
    body.extend(attribute(IFA_ADDRESS, &octets(ip)));
    let flags = if kind == RTM_NEWADDR {
        NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE
    } else {
        NLM_F_ACK
    };
    message(kind, flags, &body)
}

/// The address in an `RTM_NEWADDR` body, when it is a global-scope one on
/// link `index`.
pub(super) fn link_address(body: &[u8], index: u32) -> Option<Prefix> {
    let header = body.get(..IFADDRMSG)?;
    if header[3] != RT_SCOPE_UNIVERSE || u32_at(header, 4)? != index {
        return None;
    }
    let attrs = attributes(body.get(IFADDRMSG..)?);
    let payload = [IFA_LOCAL, IFA_ADDRESS]
        .iter()
        .find_map(|wanted| attrs.iter().find(|(kind, _)| kind == wanted))?
        .1;
    Some((address_of(header[0], payload)?, header[1]))
}

/// A `sockaddr_in` or `sockaddr_in6`, as `WGPEER_A_ENDPOINT` carries one.
fn sockaddr(endpoint: SocketAddr) -> Vec<u8> {
    let mut bytes = u16::from(family_of(endpoint.ip())).to_ne_bytes().to_vec();
    bytes.extend(endpoint.port().to_be_bytes());
    match endpoint {
        SocketAddr::V4(v4) => {
            bytes.extend(v4.ip().octets());
            bytes.extend([0u8; 8]);
        }
        SocketAddr::V6(v6) => {
            bytes.extend(v6.flowinfo().to_ne_bytes());
            bytes.extend(v6.ip().octets());
            bytes.extend(v6.scope_id().to_ne_bytes());
        }
    }
    bytes
}

fn peer_attributes(peer: &PeerSet) -> Vec<Vec<u8>> {
    let allowed: Vec<Vec<u8>> = peer
        .allowed_ips
        .iter()
        .map(|(ip, length)| {
            nested(
                0,
                &[
                    attribute(
                        WGALLOWEDIP_A_FAMILY,
                        &u16::from(family_of(*ip)).to_ne_bytes(),
                    ),
                    attribute(WGALLOWEDIP_A_IPADDR, &octets(*ip)),
                    attribute(WGALLOWEDIP_A_CIDR_MASK, &[*length]),
                ],
            )
        })
        .collect();
    let mut attrs = vec![
        attribute(WGPEER_A_PUBLIC_KEY, &peer.public_key),
        attribute(WGPEER_A_FLAGS, &WGPEER_F_REPLACE_ALLOWEDIPS.to_ne_bytes()),
        // All zeroes is none: the kernel drops the key it held.
        attribute(
            WGPEER_A_PRESHARED_KEY,
            &peer.preshared_key.unwrap_or([0; 32]),
        ),
        attribute(
            WGPEER_A_PERSISTENT_KEEPALIVE_INTERVAL,
            &peer.keepalive.unwrap_or(0).to_ne_bytes(),
        ),
    ];
    if let Some(endpoint) = peer.endpoint {
        attrs.push(attribute(WGPEER_A_ENDPOINT, &sockaddr(endpoint)));
    }
    attrs.push(nested(WGPEER_A_ALLOWEDIPS, &allowed));
    attrs
}

/// `WG_CMD_SET_DEVICE` of `device` on WireGuard link `name`, to generic
/// netlink family `family`.
pub(super) fn set_device(family: u16, name: &str, device: &DeviceSet) -> Vec<u8> {
    let mut body = vec![WG_CMD_SET_DEVICE, WG_GENL_VERSION, 0, 0];
    body.extend(attribute(WGDEVICE_A_IFNAME, &name_payload(name)));
    body.extend(attribute(WGDEVICE_A_PRIVATE_KEY, &device.private_key));
    if let Some(port) = device.listen_port {
        body.extend(attribute(WGDEVICE_A_LISTEN_PORT, &port.to_ne_bytes()));
    }
    let mut peers: Vec<Vec<u8>> = device
        .remove
        .iter()
        .map(|key| {
            nested(
                0,
                &[
                    attribute(WGPEER_A_PUBLIC_KEY, key),
                    attribute(WGPEER_A_FLAGS, &WGPEER_F_REMOVE_ME.to_ne_bytes()),
                ],
            )
        })
        .collect();
    peers.extend(
        device
            .peers
            .iter()
            .map(|peer| nested(0, &peer_attributes(peer))),
    );
    if !peers.is_empty() {
        body.extend(nested(WGDEVICE_A_PEERS, &peers));
    }
    message(family, NLM_F_ACK, &body)
}

/// `WG_CMD_SET_DEVICE` of one endpoint on WireGuard link `name`: the peer
/// with `WGPEER_F_UPDATE_ONLY` and its endpoint, and nothing else.
pub(super) fn set_endpoint(
    family: u16,
    name: &str,
    public_key: &[u8; 32],
    endpoint: SocketAddr,
) -> Vec<u8> {
    let mut body = vec![WG_CMD_SET_DEVICE, WG_GENL_VERSION, 0, 0];
    body.extend(attribute(WGDEVICE_A_IFNAME, &name_payload(name)));
    body.extend(nested(
        WGDEVICE_A_PEERS,
        &[nested(
            0,
            &[
                attribute(WGPEER_A_PUBLIC_KEY, public_key),
                attribute(WGPEER_A_FLAGS, &WGPEER_F_UPDATE_ONLY.to_ne_bytes()),
                attribute(WGPEER_A_ENDPOINT, &sockaddr(endpoint)),
            ],
        )],
    ));
    message(family, NLM_F_ACK, &body)
}

/// `RTM_NEWROUTE` or `RTM_DELROUTE` of `route` out of link `index`, in the
/// main table with [`RTPROT_MICA`]. A new one that is there already is a
/// refusal: it is not micad's to replace.
pub(super) fn route_request(kind: u16, index: u32, route: &TunnelRoute) -> Vec<u8> {
    let (destination, length) = route.destination;
    let scope = if route.gateway.is_some() {
        RT_SCOPE_UNIVERSE
    } else {
        RT_SCOPE_LINK
    };
    let mut body = vec![
        family_of(destination),
        length,
        0,
        0,
        RT_TABLE_MAIN,
        RTPROT_MICA,
        scope,
        RTN_UNICAST,
    ];
    body.extend(0u32.to_ne_bytes());
    body.extend(attribute(RTA_DST, &octets(destination)));
    body.extend(attribute(RTA_OIF, &index.to_ne_bytes()));
    if let Some(gateway) = route.gateway {
        body.extend(attribute(RTA_GATEWAY, &octets(gateway)));
    }
    if let Some(source) = route.source {
        body.extend(attribute(RTA_PREFSRC, &octets(source)));
    }
    body.extend(attribute(RTA_PRIORITY, &route.metric.to_ne_bytes()));
    let flags = if kind == RTM_NEWROUTE {
        NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL
    } else {
        NLM_F_ACK
    };
    message(kind, flags, &body)
}

/// The route in an `RTM_NEWROUTE` body with its protocol, when it is a
/// main-table unicast route out of link `index`.
pub(super) fn link_route(body: &[u8], index: u32) -> Option<(TunnelRoute, u8)> {
    let header = body.get(..RTMSG)?;
    let (family, length, protocol, kind) = (header[0], header[1], header[5], header[7]);
    let unspecified = match family {
        AF_INET => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        AF_INET6 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        _ => return None,
    };
    let mut table = u32::from(header[4]);
    let mut out_link = None;
    let mut destination = None;
    let mut route = TunnelRoute {
        destination: (unspecified, length),
        gateway: None,
        source: None,
        metric: 0,
    };
    for (attr, payload) in attributes(body.get(RTMSG..)?) {
        match attr {
            RTA_TABLE => table = u32_at(payload, 0)?,
            RTA_OIF => out_link = u32_at(payload, 0),
            RTA_DST => destination = address_of(family, payload),
            RTA_GATEWAY => route.gateway = address_of(family, payload),
            RTA_PREFSRC => route.source = address_of(family, payload),
            RTA_PRIORITY => route.metric = u32_at(payload, 0)?,
            _ => {}
        }
    }
    if table != u32::from(RT_TABLE_MAIN) || kind != RTN_UNICAST || out_link != Some(index) {
        return None;
    }
    if let Some(destination) = destination {
        route.destination = (destination, length);
    }
    Some((route, protocol))
}

#[cfg(test)]
mod tests;
