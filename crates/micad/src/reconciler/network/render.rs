//! The networkd units a network declaration renders to.

use crate::wgkeys::Keystore;
use micad_settings::{IfaceKind, IfaceSettings, WireguardConfig};
use std::path::Path;

/// Render the `[WireGuard]` and `[WireGuardPeer]` sections of a tunnel's
/// netdev.
///
/// `PrivateKeyFile=` names the key rather than carrying it: the unit file lives
/// in networkd's runtime directory, which is world-readable, and the key file
/// it points at is not.
pub(super) fn render_wireguard(cfg: &WireguardConfig, key_path: &Path) -> String {
    let mut out = format!("\n[WireGuard]\nPrivateKeyFile={}\n", key_path.display());
    if let Some(port) = cfg.listen_port {
        out.push_str(&format!("ListenPort={port}\n"));
    }
    for peer in &cfg.peers {
        out.push_str(&format!(
            "\n[WireGuardPeer]\nPublicKey={}\n",
            peer.public_key
        ));
        if !peer.allowed_ips.is_empty() {
            out.push_str(&format!("AllowedIPs={}\n", peer.allowed_ips.join(",")));
        }
        if let Some(endpoint) = &peer.endpoint {
            out.push_str(&format!("Endpoint={endpoint}\n"));
        }
        if let Some(keepalive) = peer.persistent_keepalive {
            out.push_str(&format!("PersistentKeepalive={keepalive}\n"));
        }
    }
    out
}

/// Render the `.netdev` unit that creates `iface`, for a kind that needs one.
///
/// `None` for a physical entry, whose device the kernel already has. A
/// wireguard entry's unit names its key file in `keys`, which is a path the
/// store answers for whether or not a key has been drawn yet — the key itself
/// is drawn on the way past in [`NetworkReconciler::apply`].
pub(super) fn render_netdev(iface: &str, cfg: &IfaceSettings, keys: &Keystore) -> Option<String> {
    match (cfg.kind, &cfg.vlan, &cfg.wireguard) {
        (IfaceKind::Vlan, Some(vlan), _) => Some(format!(
            "[NetDev]\nName={iface}\nKind=vlan\n\n[VLAN]\nId={}\n",
            vlan.id
        )),
        (IfaceKind::Bridge, _, _) => Some(format!("[NetDev]\nName={iface}\nKind=bridge\n")),
        (IfaceKind::Wireguard, _, Some(wireguard)) => Some(format!(
            "[NetDev]\nName={iface}\nKind=wireguard\n{}",
            render_wireguard(wireguard, &keys.key_path(iface))
        )),
        _ => None,
    }
}

/// Render one networkd unit for `iface`.
///
/// `master` is the bridge that claimed this interface as a port, and `vlans`
/// the VLAN children declared on top of it: networkd creates a VLAN only when
/// the parent's `.network` names it, so the child's existence is a fact about
/// the PARENT's unit.
pub(super) fn render_unit(
    iface: &str,
    cfg: &IfaceSettings,
    master: Option<&str>,
    vlans: &[&str],
) -> String {
    let mut out = format!("[Match]\nName={iface}\n\n[Network]\n");
    if let Some(bridge) = master {
        // A port's addressing is the bridge's; validation has already refused
        // an entry that tried to keep its own.
        out.push_str(&format!("Bridge={bridge}\n"));
    } else if cfg.dhcp {
        out.push_str("DHCP=yes\n");
        for dns in &cfg.dns {
            out.push_str(&format!("DNS={dns}\n"));
        }
    } else if let Some(static_cfg) = &cfg.static_ {
        out.push_str(&format!("Address={}\n", static_cfg.address));
        if let Some(gateway) = &static_cfg.gateway {
            out.push_str(&format!("Gateway={gateway}\n"));
        }
        for dns in &static_cfg.dns {
            out.push_str(&format!("DNS={dns}\n"));
        }
    }
    for child in vlans {
        out.push_str(&format!("VLAN={child}\n"));
    }
    // A port carries neither: its addressing is the bridge's, and a DHCP
    // server or a route on a link that has no address of its own is a
    // configuration networkd would accept and nothing could use. apid refuses
    // the entry; the renderer refuses to render it, so a file hand-edited past
    // apid still produces a unit that means what the bridge means.
    if master.is_none() {
        if cfg.dhcp_server.is_some() {
            out.push_str("DHCPServer=yes\n");
        }
        for route in &cfg.routes {
            out.push_str(&format!("\n[Route]\nDestination={}\n", route.destination));
            if let Some(gateway) = &route.gateway {
                out.push_str(&format!("Gateway={gateway}\n"));
            }
            if let Some(metric) = route.metric {
                out.push_str(&format!("Metric={metric}\n"));
            }
        }
        if let Some(server) = &cfg.dhcp_server {
            out.push_str(&format!(
                "\n[DHCPServer]\nPoolOffset={}\nPoolSize={}\n",
                server.pool_offset, server.pool_size
            ));
            for dns in &server.dns {
                out.push_str(&format!("DNS={dns}\n"));
            }
            if let Some(seconds) = server.lease_seconds {
                out.push_str(&format!("DefaultLeaseTimeSec={seconds}\n"));
            }
        }
    }
    // The entry's own servers replace the lease's, in every way a lease or a
    // router advertisement can name one.
    if master.is_none() && cfg.dhcp && !cfg.dns.is_empty() {
        out.push_str("\n[DHCPv4]\nUseDNS=no\n\n[DHCPv6]\nUseDNS=no\n\n[IPv6AcceptRA]\nUseDNS=no\n");
    }
    out
}

/// Whether `file_name` is one this reconciler wrote: `50-mica-<iface>.network`
/// or, for a virtual link, `50-mica-<iface>.netdev`.
pub(super) fn is_mica_managed(file_name: &str) -> bool {
    file_name.starts_with("50-mica-")
        && (file_name.ends_with(".network") || file_name.ends_with(".netdev"))
}

/// The interface a swept `50-mica-<iface>.netdev` created, and nothing for any
/// other file.
///
/// The swept file names are the exact set of virtual devices this reconciler
/// is giving up, which is what makes them the exact set to delete.
pub(super) fn mica_netdev_iface(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix("50-mica-")
        .and_then(|rest| rest.strip_suffix(".netdev"))
}
