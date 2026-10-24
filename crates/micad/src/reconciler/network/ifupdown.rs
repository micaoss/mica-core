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
//! Wired DHCP and static addresses with their static routes, and CAN
//! controllers set with iproute2's `ip` from their stanza: VLANs, bridges and
//! WireGuard are networkd's, and an OpenRC device refuses them by name rather
//! than half-configuring them.
//!
//! A static route on a static or manual stanza is an `up ip route add` line
//! and the matching `down ip route del`. On a DHCP stanza the link is up
//! before the lease, so each route is a `route_<n>` option that Base's udhcpc
//! script applies once the lease is bound and removes on deconfig.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use micad_settings::{CanConfig, IfaceKind, IfaceSettings, RouteConfig, Settings, StaticConfig};
use serde_json::json;

use super::{
    kind_name, route_metrics, validate_dns_override, validate_iface_name, validate_static,
};
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
/// Base's program building [`RESOLV_CONF`] from [`RESOLVERS_DIR`].
const MICA_RESOLV: &str = "/usr/lib/mica/mica-resolv";
const FILE_MODE: u32 = 0o644;
/// Where a root's `ip` may be; the first present is the one ifupdown runs.
const IP_PATHS: [&str; 4] = ["/usr/sbin/ip", "/usr/bin/ip", "/sbin/ip", "/bin/ip"];

pub struct IfupdownReconciler {
    path: PathBuf,
    net_dir: PathBuf,
    commands: Arc<dyn Commands>,
    /// iproute2's `ip`, which sets CAN parameters; `None` on a root without it.
    ip: Option<PathBuf>,
    /// The board's declared ports: an internal one's absent `internet` is
    /// `false`, and a hidden one gets no stanza unless declared.
    board: Arc<crate::board::Board>,
}

impl IfupdownReconciler {
    pub fn new(path: PathBuf, net_dir: PathBuf, commands: Arc<dyn Commands>) -> Self {
        Self {
            path,
            net_dir,
            commands,
            ip: None,
            board: Arc::default(),
        }
    }

    /// The `ip` a CAN stanza runs: iproute2's, or `None` without it.
    pub fn with_ip(mut self, ip: Option<PathBuf>) -> Self {
        self.ip = ip;
        self
    }

    /// Render with the board's declared ports.
    #[must_use]
    pub fn with_board(mut self, board: Arc<crate::board::Board>) -> Self {
        self.board = board;
        self
    }

    pub fn production() -> Self {
        Self::new(
            PathBuf::from(INTERFACES_PATH),
            PathBuf::from(NET_CLASS_DIR),
            Arc::new(Host),
        )
        .with_ip(iproute2_ip(&IP_PATHS.map(Path::new)))
        .with_board(Arc::new(crate::board::host().clone()))
    }

    /// Wired interfaces (`eth*`) the kernel has, less the ones the board
    /// hides.
    fn wired(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.net_dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("eth") && !self.board.hidden(name))
            .collect();
        names.sort();
        names
    }
}

/// The first of `paths` that exists, when it is iproute2's `ip` and not a
/// link to BusyBox, whose applet cannot set CAN parameters.
pub(super) fn iproute2_ip(paths: &[&Path]) -> Option<PathBuf> {
    let found = paths.iter().find(|path| path.exists())?;
    let resolved = std::fs::canonicalize(found).ok()?;
    (resolved.file_name()? != "busybox").then(|| found.to_path_buf())
}

/// Refuse what ifupdown here cannot configure, naming it.
fn validate(iface: &str, cfg: &IfaceSettings, ip: Option<&Path>) -> Result<()> {
    validate_iface_name(iface)?;
    if cfg.kind == IfaceKind::Can {
        micad_settings::validate_can(iface, cfg).map_err(anyhow::Error::msg)?;
        if ip.is_none() {
            bail!(
                "network.{iface}: a CAN interface needs iproute2's ip, which this OpenRC root does not carry"
            );
        }
        return Ok(());
    }
    validate_dns_override(iface, cfg)?;
    let refused = if !matches!(cfg.kind, IfaceKind::Physical) {
        Some(kind_name(cfg.kind))
    } else if cfg.vlan.is_some() {
        Some("vlan")
    } else if cfg.bridge.is_some() {
        Some("bridge")
    } else if cfg.wireguard.is_some() {
        Some("wireguard")
    } else if cfg.can.is_some() {
        Some("can")
    } else {
        None
    };
    if let Some(what) = refused {
        bail!(
            "network.{iface}: {what} needs systemd-networkd, which this OpenRC device does not run"
        );
    }
    if let Some(("routes", reason)) = micad_settings::routing_problem(cfg) {
        bail!("network.{iface}.routes: {reason}");
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

/// A DHCP client on `iface`. With servers of its own in `dns`, the lease is
/// asked for no DNS option and the servers go into a resolver file that sorts
/// before the lease's, so they are the ones a resolver reads first even from a
/// server that names its own unasked.
///
/// `metric` is the lease's default-route metric. ifupdown hands every stanza
/// option to the commands it runs as `IF_<OPTION>`, so udhcpc and Base's
/// script see it as `IF_METRIC`.
fn dhcp_stanza(out: &mut String, iface: &str, dns: &[String], metric: Option<u32>, internet: bool) {
    let _ = write!(
        out,
        "\nauto {iface}\niface {iface} inet dhcp\n    script {UDHCPC_SCRIPT}\n"
    );
    if let Some(metric) = metric {
        let _ = writeln!(out, "    metric {metric}");
    }
    // Base's udhcpc script reads it as `IF_INTERNET` and then sets no
    // default route from the lease.
    if !internet {
        let _ = writeln!(out, "    internet 0");
    }
    if dns.is_empty() {
        let _ = writeln!(out, "    udhcpc_opts -b -S");
        return;
    }
    let _ = writeln!(
        out,
        "    udhcpc_opts -b -S -o -O subnet -O router -O broadcast"
    );
    resolver_lines(out, &format!("00-{iface}"), dns);
}

/// The `up` and `down` lines that put `dns` into the resolver file `name` and
/// rebuild the resolver configuration: with Base's `mica-resolv`, which lets a
/// global file stand alone, or else from every file.
fn resolver_lines(out: &mut String, name: &str, dns: &[String]) {
    let rebuild = format!(
        "{{ if [ -x {MICA_RESOLV} ]; then {MICA_RESOLV}; else {{ cat {RESOLVERS_DIR}/* 2>/dev/null || :; }} >{RESOLV_CONF}; fi; }}"
    );
    let _ = writeln!(
        out,
        "    up mkdir -p {RESOLVERS_DIR} && printf 'nameserver %s\\n' {} >{RESOLVERS_DIR}/{name} && {rebuild}",
        dns.join(" ")
    );
    let _ = writeln!(out, "    down rm -f {RESOLVERS_DIR}/{name} && {rebuild}");
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
        resolver_lines(out, iface, &cfg.dns);
    }
    Ok(())
}

/// Each of `routes` on `iface`'s stanza. A static or manual stanza adds each
/// once the link is up and removes it before it goes down; the removal may
/// find the route gone with the link, so it never fails the stanza. A DHCP
/// stanza's link is up before its lease, so there each route is a
/// `route_<n>` option instead, `<destination> [via <gateway>] [metric <m>]`,
/// which Base's udhcpc script reads as `IF_ROUTE_<n>` and applies with
/// `dev <iface>` once the lease is bound (section 3.8 of docs/mica-core.md).
/// Only parsed addresses reach the line.
fn route_lines(out: &mut String, iface: &str, routes: &[RouteConfig], dhcp: bool) -> Result<()> {
    for (index, route) in routes.iter().enumerate() {
        let (destination, prefix) = address_and_prefix(&route.destination)?;
        let mut spec = format!("{destination}/{prefix}");
        if let Some(gateway) = &route.gateway {
            let _ = write!(spec, " via {}", gateway.parse::<IpAddr>()?);
        }
        if dhcp {
            if let Some(metric) = route.metric {
                let _ = write!(spec, " metric {metric}");
            }
            let _ = writeln!(out, "    route_{} {spec}", index + 1);
            continue;
        }
        let _ = write!(spec, " dev {iface}");
        if let Some(metric) = route.metric {
            let _ = write!(spec, " metric {metric}");
        }
        let _ = writeln!(out, "    up ip route add {spec}");
        let _ = writeln!(out, "    down ip route del {spec} || :");
    }
    Ok(())
}

/// A CAN controller: no address, its parameters set by `ip` while the link
/// is down, then the link brought up.
fn can_stanza(out: &mut String, iface: &str, can: &CanConfig, ip: &Path) {
    let ip = ip.display();
    let mut set = format!("{ip} link set dev {iface} type can bitrate {}", can.bitrate);
    if let Some(ms) = can.restart_ms {
        let _ = write!(set, " restart-ms {ms}");
    }
    match (can.fd, can.data_bitrate) {
        (Some(true), Some(rate)) => {
            let _ = write!(set, " fd on dbitrate {rate}");
        }
        _ => set.push_str(" fd off"),
    }
    let _ = write!(
        out,
        "\nauto {iface}\niface {iface} inet manual\n    pre-up {set}\n    up {ip} link set dev {iface} up\n    down {ip} link set dev {iface} down\n"
    );
}

/// The file for `network`, with DHCP on every wired interface it does not
/// declare, as mica-systemd's `80-dhcp.network` gives it, and each DHCP link's
/// route metric as networkd's units carry it. `ip` is the one a CAN stanza
/// runs; `board` keeps an internal port it does not declare off the internet.
pub(super) fn render(
    network: &BTreeMap<String, IfaceSettings>,
    wired: &[String],
    ip: Option<&Path>,
    board: &crate::board::Board,
) -> Result<String> {
    let mut out = String::from(
        "# Managed by micad from `network`; the device's network. Do not edit.\n\
         auto lo\niface lo inet loopback\n",
    );
    let ports: BTreeSet<String> = wired
        .iter()
        .filter(|iface| !network.contains_key(*iface))
        .cloned()
        .collect();
    let metrics = route_metrics(network, &BTreeMap::new(), &ports);
    for (iface, cfg) in network {
        if let (Some(can), Some(ip)) = (&cfg.can, ip) {
            can_stanza(&mut out, iface, can, ip);
            continue;
        }
        match &cfg.static_ {
            Some(static_) => static_stanza(&mut out, iface, static_)?,
            None if cfg.dhcp => dhcp_stanza(
                &mut out,
                iface,
                &cfg.dns,
                metrics.get(iface.as_str()).copied(),
                cfg.internet(),
            ),
            None => {
                let _ = write!(out, "\nauto {iface}\niface {iface} inet manual\n");
            }
        }
        route_lines(
            &mut out,
            iface,
            &cfg.routes,
            cfg.static_.is_none() && cfg.dhcp,
        )?;
    }
    for iface in &ports {
        dhcp_stanza(
            &mut out,
            iface,
            &[],
            metrics.get(iface.as_str()).copied(),
            !board.internal(iface),
        );
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
            validate(iface, cfg, self.ip.as_deref())?;
        }
        let rendered = render(
            &self.board.effective(&settings.network),
            &self.wired(),
            self.ip.as_deref(),
            &self.board,
        )?;
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
