//! The rules a network declaration is held to before anything is written.

use micad_settings::{IfaceKind, IfaceSettings, is_ip_or_cidr};
use std::collections::BTreeMap;

/// Refuse an interface name the kernel could not have and the renderer must
/// not see.
pub(super) fn validate_iface_name(iface: &str) -> anyhow::Result<()> {
    micad_settings::check_iface_name("network interface", iface).map_err(anyhow::Error::msg)
}

/// Refuse a static configuration whose values could not be addresses.
pub(super) fn validate_static(
    iface: &str,
    cfg: &micad_settings::StaticConfig,
) -> anyhow::Result<()> {
    if !is_ip_or_cidr(&cfg.address) {
        return Err(anyhow::anyhow!(
            "network.{iface} static address {:?} is not an IP address or CIDR",
            cfg.address
        ));
    }
    if let Some(gateway) = &cfg.gateway
        && gateway.parse::<std::net::IpAddr>().is_err()
    {
        return Err(anyhow::anyhow!(
            "network.{iface} gateway {gateway:?} is not an IP address"
        ));
    }
    for dns in &cfg.dns {
        if dns.parse::<std::net::IpAddr>().is_err() {
            return Err(anyhow::anyhow!(
                "network.{iface} DNS server {dns:?} is not an IP address"
            ));
        }
    }
    Ok(())
}

/// The spelling a kind has in the settings file, which is also the name of the
/// block that belongs to it.
pub(super) fn kind_name(kind: IfaceKind) -> &'static str {
    match kind {
        IfaceKind::Physical => "physical",
        IfaceKind::Vlan => "vlan",
        IfaceKind::Bridge => "bridge",
        IfaceKind::Wireguard => "wireguard",
    }
}

/// Refuse DNS servers of its own on an entry that is not a DHCP client, and
/// servers that are not addresses.
pub(super) fn validate_dns_override(iface: &str, cfg: &IfaceSettings) -> anyhow::Result<()> {
    if cfg.dns.is_empty() {
        return Ok(());
    }
    if !cfg.dhcp || cfg.static_.is_some() {
        return Err(anyhow::anyhow!(
            "network.{iface} names DNS servers in `dns`, which replaces the servers of a DHCP lease; a static entry names them in `static.dns`"
        ));
    }
    for dns in &cfg.dns {
        if dns.parse::<std::net::IpAddr>().is_err() {
            return Err(anyhow::anyhow!(
                "network.{iface} DNS server {dns:?} is not an IP address"
            ));
        }
    }
    Ok(())
}

/// Refuse an entry whose optional blocks disagree with its `kind`.
pub(super) fn validate_kind_blocks(iface: &str, cfg: &IfaceSettings) -> anyhow::Result<()> {
    let kind = kind_name(cfg.kind);
    let own = (!matches!(cfg.kind, IfaceKind::Physical)).then_some(kind);
    for (block, present) in [
        ("vlan", cfg.vlan.is_some()),
        ("bridge", cfg.bridge.is_some()),
        ("wireguard", cfg.wireguard.is_some()),
    ] {
        if present && own != Some(block) {
            return Err(anyhow::anyhow!(
                "network.{iface} is kind {kind} but carries a {block} block"
            ));
        }
    }
    let missing = match cfg.kind {
        IfaceKind::Vlan => cfg.vlan.is_none(),
        IfaceKind::Bridge => cfg.bridge.is_none(),
        IfaceKind::Wireguard => cfg.wireguard.is_none(),
        IfaceKind::Physical => false,
    };
    if missing {
        return Err(anyhow::anyhow!(
            "network.{iface} is kind {kind} but carries no {kind} block"
        ));
    }
    Ok(())
}

/// Refuse a `network` subtree the renderer must not see, before any of it is
/// written.
pub(super) fn validate_network(network: &BTreeMap<String, IfaceSettings>) -> anyhow::Result<()> {
    for (iface, cfg) in network {
        validate_iface_name(iface)?;
        if let Some(static_cfg) = &cfg.static_ {
            validate_static(iface, static_cfg)?;
        }
        validate_dns_override(iface, cfg)?;
        validate_kind_blocks(iface, cfg)?;
        if let Some(wireguard) = &cfg.wireguard {
            micad_settings::validate_peers(iface, &wireguard.peers).map_err(anyhow::Error::msg)?;
        }
    }
    micad_settings::validate_topology(network).map_err(anyhow::Error::msg)
}

/// Which bridge each declared port belongs to.
pub(super) fn bridge_ports(network: &BTreeMap<String, IfaceSettings>) -> BTreeMap<&str, &str> {
    let mut ports = BTreeMap::new();
    for (iface, cfg) in network {
        if let Some(bridge) = &cfg.bridge {
            for port in &bridge.ports {
                ports.insert(port.as_str(), iface.as_str());
            }
        }
    }
    ports
}

/// The declared VLAN children of each parent.
pub(super) fn vlan_children(
    network: &BTreeMap<String, IfaceSettings>,
) -> BTreeMap<&str, Vec<&str>> {
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (iface, cfg) in network {
        if let Some(vlan) = &cfg.vlan {
            children
                .entry(vlan.parent.as_str())
                .or_default()
                .push(iface.as_str());
        }
    }
    children
}
