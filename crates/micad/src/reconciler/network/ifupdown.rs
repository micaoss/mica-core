//! The network of an OpenRC root: `/etc/network/interfaces`, applied by busybox
//! ifupdown the way Alpine configures one.
//!
//! Base links `/etc/network/interfaces` to `/var/lib/mica/network/interfaces`
//! on DATA, and its `mica-network` service runs `ifup -a` at boot; micad
//! renders that one file and, when it changes, takes the network down on the
//! old file (`ifdown -a`), writes the new one and brings it up (`ifup -a`), so
//! a DHCP client the old file started is stopped by the stanza that started
//! it. DHCP is Base's busybox udhcpc and its script; a static address carries
//! its DNS servers into Base's `/run/mica/resolv.d/<iface>`.
//!
//! Wired DHCP and static addresses only: VLANs, bridges, WireGuard, static
//! routes and the DHCP server are networkd's, and an OpenRC device refuses
//! them by name rather than half-configuring them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use micad_settings::{IfaceKind, IfaceSettings, Settings, StaticConfig};
use serde_json::json;

use super::{kind_name, validate_iface_name, validate_static};
use crate::openrc::{Commands, Host};
use crate::reconciler::Reconciler;

/// Where micad writes the file `/etc/network/interfaces` links to.
const INTERFACES_PATH: &str = "/var/lib/mica/network/interfaces";
/// Network interfaces, whose wired ones get DHCP unless declared.
const NET_CLASS_DIR: &str = "/sys/class/net";
/// Base's udhcpc script: the lease on the link, DNS under /run.
const UDHCPC_SCRIPT: &str = "/usr/lib/mica/mica-udhcpc";
/// Base's per-interface resolver files and the file built from them.
const RESOLVERS_DIR: &str = "/run/mica/resolv.d";
const RESOLV_CONF: &str = "/run/mica/resolv.conf";
const FILE_MODE: u32 = 0o644;

pub struct IfupdownReconciler {
    path: PathBuf,
    net_dir: PathBuf,
    commands: Arc<dyn Commands>,
}

impl IfupdownReconciler {
    pub fn new(path: PathBuf, net_dir: PathBuf, commands: Arc<dyn Commands>) -> Self {
        Self {
            path,
            net_dir,
            commands,
        }
    }

    pub fn production() -> Self {
        Self::new(
            PathBuf::from(INTERFACES_PATH),
            PathBuf::from(NET_CLASS_DIR),
            Arc::new(Host),
        )
    }

    /// Wired interfaces (`eth*`) the kernel has.
    fn wired(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.net_dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("eth"))
            .collect();
        names.sort();
        names
    }
}

/// Refuse what ifupdown here cannot configure, naming it.
fn validate(iface: &str, cfg: &IfaceSettings) -> Result<()> {
    validate_iface_name(iface)?;
    let refused = if !matches!(cfg.kind, IfaceKind::Physical) {
        Some(kind_name(cfg.kind))
    } else if cfg.vlan.is_some() {
        Some("vlan")
    } else if cfg.bridge.is_some() {
        Some("bridge")
    } else if cfg.wireguard.is_some() {
        Some("wireguard")
    } else if !cfg.routes.is_empty() {
        Some("routes")
    } else if cfg.dhcp_server.is_some() {
        Some("dhcpServer")
    } else {
        None
    };
    if let Some(what) = refused {
        bail!(
            "network.{iface}: {what} needs systemd-networkd, which this OpenRC device does not run"
        );
    }
    if let Some(static_) = &cfg.static_ {
        if cfg.dhcp {
            bail!(
                "network.{iface}: DHCP and a static address together need systemd-networkd; choose one"
            );
        }
        validate_static(iface, static_)?;
        let (address, _) = address_and_prefix(&static_.address)?;
        if let Some(gateway) = &static_.gateway
            && gateway.parse::<IpAddr>()?.is_ipv4() != address.is_ipv4()
        {
            bail!("network.{iface}: gateway {gateway} is not of the address's family");
        }
    }
    Ok(())
}

fn address_and_prefix(cidr: &str) -> Result<(IpAddr, u8)> {
    let (address, prefix) = cidr.split_once('/').unwrap_or((cidr, ""));
    let address: IpAddr = address.parse()?;
    let prefix = if prefix.is_empty() {
        if address.is_ipv4() { 32 } else { 128 }
    } else {
        prefix.parse()?
    };
    Ok((address, prefix))
}

fn netmask_v4(prefix: u8) -> String {
    let bits = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    std::net::Ipv4Addr::from(bits).to_string()
}

fn dhcp_stanza(out: &mut String, iface: &str) {
    let _ = write!(
        out,
        "\nauto {iface}\niface {iface} inet dhcp\n    script {UDHCPC_SCRIPT}\n    udhcpc_opts -b -S\n"
    );
}

fn static_stanza(out: &mut String, iface: &str, cfg: &StaticConfig) -> Result<()> {
    let (address, prefix) = address_and_prefix(&cfg.address)?;
    if address.is_ipv4() {
        let _ = write!(
            out,
            "\nauto {iface}\niface {iface} inet static\n    address {address}\n    netmask {}\n",
            netmask_v4(prefix)
        );
    } else {
        let _ = write!(
            out,
            "\nauto {iface}\niface {iface} inet6 static\n    address {address}\n    netmask {prefix}\n"
        );
    }
    if let Some(gateway) = &cfg.gateway {
        let _ = writeln!(out, "    gateway {gateway}");
    }
    if !cfg.dns.is_empty() {
        let rebuild = format!("{{ cat {RESOLVERS_DIR}/* 2>/dev/null || :; }} >{RESOLV_CONF}");
        let _ = writeln!(
            out,
            "    up mkdir -p {RESOLVERS_DIR} && printf 'nameserver %s\\n' {} >{RESOLVERS_DIR}/{iface} && {rebuild}",
            cfg.dns.join(" ")
        );
        let _ = writeln!(out, "    down rm -f {RESOLVERS_DIR}/{iface} && {rebuild}");
    }
    Ok(())
}

/// The file for `network`, with DHCP on every wired interface it does not
/// declare, as mica-systemd's `80-dhcp.network` gives it.
pub(super) fn render(
    network: &BTreeMap<String, IfaceSettings>,
    wired: &[String],
) -> Result<String> {
    let mut out = String::from(
        "# Managed by micad from `network`; the device's network. Do not edit.\n\
         auto lo\niface lo inet loopback\n",
    );
    for (iface, cfg) in network {
        match &cfg.static_ {
            Some(static_) => static_stanza(&mut out, iface, static_)?,
            None if cfg.dhcp => dhcp_stanza(&mut out, iface),
            None => {
                let _ = write!(out, "\nauto {iface}\niface {iface} inet manual\n");
            }
        }
    }
    for iface in wired.iter().filter(|iface| !network.contains_key(*iface)) {
        dhcp_stanza(&mut out, iface);
    }
    Ok(out)
}

#[async_trait::async_trait]
impl Reconciler for IfupdownReconciler {
    fn name(&self) -> &'static str {
        "network"
    }

    fn subtree(&self) -> &'static str {
        "network"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        for (iface, cfg) in &settings.network {
            validate(iface, cfg)?;
        }
        let rendered = render(&settings.network, &self.wired())?;
        if std::fs::read_to_string(&self.path).ok().as_deref() != Some(rendered.as_str()) {
            let down = self.commands.run("ifdown", &["-a"]).await?;
            if down.code != 0 {
                tracing::warn!(
                    stderr = down.stderr.trim(),
                    "ifdown -a on the previous network failed"
                );
            }
            crate::fswrite::write_config_if_changed(&self.path, &rendered, FILE_MODE)?;
            self.commands
                .run("ifup", &["-a"])
                .await?
                .success("ifup -a")?;
        }
        let file = self.path.display().to_string();
        Ok(serde_json::Value::Object(
            settings
                .network
                .iter()
                .map(|(iface, cfg)| {
                    (
                        iface.clone(),
                        json!({ "file": file, "dhcp": cfg.dhcp, "kind": kind_name(cfg.kind) }),
                    )
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests;
