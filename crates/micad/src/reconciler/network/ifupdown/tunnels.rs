//! The WireGuard tunnels of an OpenRC root, which micad runs itself over
//! netlink: ifupdown's file never names them.
//!
//! Each apply brings the kernel to the settings and changes only what
//! differs, so a micad restart leaves a healthy tunnel and its handshakes
//! alone:
//! - a link micad created ([`crate::netlink::tunnel::LINK_ALIAS`]) that no entry names is deleted, and
//!   a missing one is created;
//! - the device takes its key and port and every peer whole, in place: a peer
//!   the entry dropped is removed by itself, so the others keep their
//!   sessions;
//! - the static address is the link's only global one;
//! - the routes are the ones networkd would add for the entry (its gateway,
//!   its routes and [`micad_settings::wireguard_routes`]), with
//!   [`RTPROT_MICA`]. A stale route of micad's is deleted; a route of any
//!   other protocol is left as it is, and one already holding a wanted
//!   route's place keeps it.
//!
//! A peer's endpoint is set when it is a numeric `address:port`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use micad_settings::{IfaceKind, IfaceSettings, WireguardConfig};
use serde_json::{Value, json};

use super::address_and_prefix;
use crate::netlink::tunnel::{
    DeviceSet, PeerSet, Prefix, RTPROT_MICA, TunnelKernel, TunnelRoute, kernel_metric,
};
use crate::wgkeys::Keystore;

pub struct Tunnels {
    kernel: Arc<dyn TunnelKernel>,
    keys: Keystore,
}

impl Tunnels {
    pub fn new(kernel: Arc<dyn TunnelKernel>, keys: Keystore) -> Self {
        Self { kernel, keys }
    }

    /// Bring the kernel's tunnels to the `kind: wireguard` entries of
    /// `network`, and return each one's state.
    pub(super) fn apply(
        &self,
        network: &BTreeMap<String, IfaceSettings>,
    ) -> Result<BTreeMap<String, Value>> {
        let wanted: BTreeMap<&String, (&IfaceSettings, &WireguardConfig)> = network
            .iter()
            .filter(|(_, cfg)| cfg.kind == IfaceKind::Wireguard)
            .filter_map(|(iface, cfg)| Some((iface, (cfg, cfg.wireguard.as_ref()?))))
            .collect();
        let links = self.kernel.owned_links()?;
        for (iface, index) in &links {
            if !wanted.contains_key(iface) {
                self.kernel
                    .delete(*index)
                    .with_context(|| format!("delete the tunnel {iface}"))?;
                tracing::info!(%iface, "deleted a tunnel no entry names");
            }
        }
        let mut state = BTreeMap::new();
        for (iface, (cfg, wireguard)) in &wanted {
            let index = self
                .tunnel(iface, cfg, wireguard, links.get(*iface).copied())
                .with_context(|| format!("network.{iface}"))?;
            state.insert(
                (*iface).clone(),
                json!({
                    "dhcp": cfg.dhcp,
                    "kind": "wireguard",
                    "publicKey": self.keys.ensure(iface)?,
                    "link": index,
                }),
            );
        }
        let names: BTreeSet<&str> = wanted.keys().map(|iface| iface.as_str()).collect();
        for iface in self.keys.retain_keys(&names)? {
            tracing::info!(%iface, "removed the private key of a deleted tunnel");
        }
        Ok(state)
    }

    /// Bring one tunnel to `cfg`, creating its link unless micad already
    /// holds it as `existing`, and return the link's index.
    fn tunnel(
        &self,
        iface: &str,
        cfg: &IfaceSettings,
        wireguard: &WireguardConfig,
        existing: Option<u32>,
    ) -> Result<u32> {
        let (index, held) = match existing {
            Some(index) => (index, self.kernel.peers(iface)?),
            None => (self.kernel.create(iface)?, Vec::new()),
        };
        let peers = wireguard
            .peers
            .iter()
            .map(peer_set)
            .collect::<Result<Vec<_>>>()?;
        let remove = held
            .iter()
            .filter_map(|key| key_bytes(key))
            .filter(|key| !peers.iter().any(|peer| peer.public_key == *key))
            .collect();
        self.keys.with_private_key(iface, |private_key| {
            self.kernel.set_device(
                iface,
                &DeviceSet {
                    private_key: *private_key,
                    listen_port: wireguard.listen_port,
                    peers,
                    remove,
                },
            )
        })??;
        let address = match &cfg.static_ {
            Some(static_) => Some(address_and_prefix(&static_.address)?),
            None => None,
        };
        for held in self.kernel.addresses(index)? {
            if Some(held) != address {
                self.kernel.delete_address(index, held)?;
            }
        }
        if let Some(address) = address
            && !self.kernel.addresses(index)?.contains(&address)
        {
            self.kernel.add_address(index, address)?;
        }
        self.kernel.up(index)?;
        let wanted = routes(cfg)?;
        let held = self.kernel.routes(index)?;
        for (route, protocol) in &held {
            if *protocol == RTPROT_MICA && !wanted.contains(route) {
                self.kernel.delete_route(index, route)?;
            }
        }
        for route in &wanted {
            if held.iter().any(|(held, _)| held == route) {
                continue;
            }
            match self.kernel.add_route(index, route) {
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    tracing::warn!(
                        %iface,
                        destination = %format!("{}/{}", route.destination.0, route.destination.1),
                        "a route that is not micad's holds this tunnel route's place"
                    );
                }
                result => result?,
            }
        }
        Ok(index)
    }
}

/// The 32 bytes of a base64 key.
fn key_bytes(key: &str) -> Option<[u8; 32]> {
    BASE64.decode(key).ok()?.try_into().ok()
}

fn peer_set(peer: &micad_settings::WireguardPeer) -> Result<PeerSet> {
    let public_key = key_bytes(&peer.public_key)
        .ok_or_else(|| anyhow!("peer {} is not a WireGuard key", peer.public_key))?;
    let preshared_key = match &peer.preshared_key {
        Some(key) => Some(key_bytes(key).ok_or_else(|| {
            anyhow!(
                "the pre-shared key of peer {} is not a key",
                peer.public_key
            )
        })?),
        None => None,
    };
    Ok(PeerSet {
        public_key,
        preshared_key,
        endpoint: peer
            .endpoint
            .as_deref()
            .and_then(|endpoint| endpoint.parse::<SocketAddr>().ok()),
        keepalive: peer.persistent_keepalive,
        allowed_ips: peer
            .allowed_ips
            .iter()
            .map(|prefix| address_and_prefix(prefix))
            .collect::<Result<_>>()?,
    })
}

/// The routes networkd adds for `cfg`'s `.network`: the gateway's default
/// route, the declared routes, and the peers' prefixes.
fn routes(cfg: &IfaceSettings) -> Result<Vec<TunnelRoute>> {
    let mut routes = Vec::new();
    if let Some(gateway) = cfg.static_.as_ref().and_then(|s| s.gateway.as_deref()) {
        let gateway: IpAddr = gateway.parse()?;
        let any = if gateway.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        routes.push(route((any, 0), Some(gateway), None, None));
    }
    for declared in &cfg.routes {
        let gateway = declared
            .gateway
            .as_deref()
            .map(str::parse::<IpAddr>)
            .transpose()?;
        routes.push(route(
            address_and_prefix(&declared.destination)?,
            gateway,
            None,
            declared.metric,
        ));
    }
    for peer in micad_settings::wireguard_routes(cfg) {
        let source = peer
            .preferred_source
            .as_deref()
            .map(str::parse::<IpAddr>)
            .transpose()?;
        routes.push(route(
            address_and_prefix(&peer.destination)?,
            None,
            source,
            None,
        ));
    }
    Ok(routes)
}

fn route(
    destination: Prefix,
    gateway: Option<IpAddr>,
    source: Option<IpAddr>,
    metric: Option<u32>,
) -> TunnelRoute {
    TunnelRoute {
        destination,
        gateway,
        source,
        metric: kernel_metric(destination.0, metric),
    }
}

#[cfg(test)]
mod tests;
