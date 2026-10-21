//! Read-only observation of the network state owned by systemd-networkd,
//! wpa_supplicant and systemd-resolved.
//!
//! Configuration remains in the settings tree. This module reports what the
//! running network stack actually sees, so callers never have to infer link
//! health from generated `.network` files. Two reads are served:
//!
//! **Absence is data.** networkd not answering leaves `interfaces` absent
//! with the reason and still reports the radios; a wireless interface with no
//! control socket reports that it could not be asked; a board with no radio
//! says `supported: false`. Nothing is inferred to be healthy.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use serde_json::{Map, Value, json};

use crate::openrc::Commands;

mod adopt;
mod capabilities;
mod dns;
mod json;
mod probe;
mod radio;
mod wifi;
pub use capabilities::*;
pub use dns::*;
pub use json::*;
pub use probe::*;
pub use radio::*;
pub use wifi::*;

/// How long networkd's `Describe` may take.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one wpa_supplicant control-socket exchange may take.
pub const WPA_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the DNS probe may take, end to end.
pub const DNS_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// The name the DNS probe resolves: timesyncd's fallback pool, an outbound
/// dependency the image already has, so the probe adds no new one.
pub const DNS_PROBE_NAME: &str = "0.debian.pool.ntp.org";
/// wpa_supplicant's control-socket directory, relative to the root.
pub const WPA_CONTROL_DIR: &str = "run/wpa_supplicant";

/// hostapd's control-socket directory, relative to the root.
///
/// The `ctrl_interface=` the AP reconciler renders. hostapd speaks the same
/// request/reply protocol wpa_supplicant does, so the same client asks it.
pub const HOSTAPD_CONTROL_DIR: &str = "run/hostapd";

/// How long a scan is given to sweep before its results are read.
///
/// A 2.4 GHz sweep is a few hundred milliseconds per channel; two seconds is
/// enough for the common case and short enough that the request answers.
/// Results from a previous scan are returned either way, so a slow radio
/// yields a slightly stale list rather than an empty one.
const SCAN_SETTLE: Duration = Duration::from_secs(2);

/// The most stations one access point is walked for.
///
/// A bound and not a guess at a real device's client count: the walk is
/// `STA-FIRST` and then `STA-NEXT` per station, so an unbounded one is an
/// unbounded number of socket round trips inside an observation an operator
/// is waiting on.
const MAX_STATIONS: usize = 64;
/// Network interfaces, relative to the root.
pub const NET_CLASS_DIR: &str = "sys/class/net";
/// Bluetooth adapters, relative to the root.
pub const BLUETOOTH_CLASS_DIR: &str = "sys/class/bluetooth";
/// The most wireless interfaces asked about; each costs a socket exchange.
pub const MAX_WIFI_INTERFACES: usize = 4;
/// The most links the kernel is asked about over netlink; each costs a
/// socket exchange.
const MAX_KERNEL_LINKS: usize = 32;

#[async_trait::async_trait]
pub trait NetworkState: Send + Sync {
    /// The reduced per-interface view networkd's `Describe` yields.
    async fn describe(&self) -> anyhow::Result<Value>;
    /// The observed network state, rendered ([`observed_json`]).
    async fn observe(&self) -> anyhow::Result<Value>;
}

/// Safe default for tests and dry-run daemons.
pub struct UnavailableNetworkState;

#[async_trait::async_trait]
impl NetworkState for UnavailableNetworkState {
    async fn describe(&self) -> anyhow::Result<Value> {
        anyhow::bail!("network observation is not configured")
    }

    async fn observe(&self) -> anyhow::Result<Value> {
        anyhow::bail!("network observation is not configured")
    }
}

/// Production observer backed by `org.freedesktop.network1.Manager.Describe`,
/// wpa_supplicant's control sockets and resolved.
pub struct SystemdNetworkState {
    root: PathBuf,
    probe_dns: bool,
    /// What `iw` is asked through; none in a fixture tree.
    commands: Option<Arc<dyn Commands>>,
    /// The radio's capabilities, asked once: they do not change while the
    /// daemon runs.
    capabilities: OnceLock<Value>,
}

impl SystemdNetworkState {
    /// The observer `main.rs` attaches on a device: rooted at `/`, DNS
    /// probe on.
    #[must_use]
    pub fn production() -> Self {
        Self {
            commands: Some(Arc::new(crate::openrc::Host)),
            ..Self::at("/").with_dns_probe(true)
        }
    }

    /// An observer rooted at `root`, with NO DNS probe: nothing here touches
    /// the network until the probe is switched on, so a fixture tree cannot
    /// make a test resolve a name against the machine running it.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            probe_dns: false,
            commands: None,
            capabilities: OnceLock::new(),
        }
    }

    /// Whether [`NetworkState::observe`] runs the resolved probe.
    #[must_use]
    pub fn with_dns_probe(mut self, probe: bool) -> Self {
        self.probe_dns = probe;
        self
    }
}

/// Each wireless interface's association (wpa_supplicant's control socket)
/// and access point (hostapd's), under `root`.
pub(crate) async fn observe_wifi(root: &std::path::Path, interfaces: &[String]) -> WifiEvidence {
    let control_dir = root.join(WPA_CONTROL_DIR);
    let mut evidence = WifiEvidence {
        control_dir_present: control_dir.is_dir(),
        ..WifiEvidence::default()
    };
    for interface in interfaces.iter().take(MAX_WIFI_INTERFACES) {
        let dir = control_dir.clone();
        let name = interface.clone();
        let status =
            tokio::task::spawn_blocking(move || wpa_query(&dir, &name, "STATUS", WPA_TIMEOUT))
                .await
                .map_err(|err| std::io::Error::other(err.to_string()))
                .and_then(|inner| inner);
        match status {
            Ok(text) => {
                let mut association = parse_wpa_status(interface, &text);
                let dir = control_dir.clone();
                let name = interface.clone();
                if let Ok(Ok(poll)) = tokio::task::spawn_blocking(move || {
                    wpa_query(&dir, &name, "SIGNAL_POLL", WPA_TIMEOUT)
                })
                .await
                {
                    apply_signal_poll(&mut association, &poll);
                }
                evidence.associations.push(association);
            }
            Err(err) => evidence.associations.push(WifiAssociation {
                interface: interface.clone(),
                detail: Some(format!("wpa_supplicant could not be asked: {err}")),
                ..WifiAssociation::default()
            }),
        }
    }
    // The other end of the same radio: what this device is associated
    // with above, and what is associated with this device here. Only an
    // interface hostapd answered for is reported -- a station radio has no
    // control socket, and an access point with no clients is a different
    // fact from an interface that is not one.
    for interface in interfaces.iter().take(MAX_WIFI_INTERFACES) {
        let root = root.to_path_buf();
        let name = interface.clone();
        if let Ok(access_point) =
            tokio::task::spawn_blocking(move || observe_access_point(&root, &name)).await
            && access_point.available
        {
            evidence.access_points.push(access_point);
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("proc/net/wireless")) {
        let levels = parse_proc_net_wireless(&text);
        for association in &mut evidence.associations {
            if association.rssi_dbm.is_none() {
                association.rssi_dbm = levels.get(&association.interface).copied();
            }
        }
    }
    evidence
}

/// The first radio's capabilities ([`wifi_capabilities`]), from `cache` once
/// `iw` has answered.
pub(crate) async fn cached_capabilities(
    cache: &OnceLock<Value>,
    root: &std::path::Path,
    commands: &dyn Commands,
    radios: &RadioEvidence,
) -> Option<Value> {
    if let Some(known) = cache.get() {
        return Some(known.clone());
    }
    let interface = radios.wifi_interfaces.first()?;
    let found = wifi_capabilities(root, commands, interface).await?;
    Some(cache.get_or_init(|| found).clone())
}

/// What the observer asked the kernel about particular links: the clock
/// networkd's lifetimes are read against, and each CAN and WireGuard link's
/// own state, rendered.
#[derive(Debug, Clone, Default)]
pub struct LinkEvidence {
    /// `CLOCK_BOOTTIME` when the observation ran, in microseconds: networkd's
    /// address lifetimes are timestamps on that clock.
    pub boottime_usec: Option<u64>,
    /// Each CAN link's controller state ([`crate::netlink::can_json`]), or
    /// why it could not be read.
    pub can: BTreeMap<String, Value>,
    /// Each WireGuard link's `{available, peers}`, or why they could not be
    /// read.
    pub wireguard: BTreeMap<String, Value>,
    /// Each link configured by a file that is not micad's: the entry that
    /// keeps that configuration ([`adopt::adoption`]).
    pub adoptions: BTreeMap<String, Value>,
}

/// Ask the kernel about the CAN and WireGuard links `describe` lists.
pub(crate) async fn observe_links(describe: Result<&Value, &str>) -> LinkEvidence {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    let mut evidence = LinkEvidence {
        boottime_usec: u64::try_from(now.tv_sec)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000))
            .and_then(|usec| usec.checked_add(u64::try_from(now.tv_nsec / 1000).ok()?)),
        ..LinkEvidence::default()
    };
    let links = describe
        .ok()
        .and_then(|value| value.get("Interfaces"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for link in links.iter().take(MAX_KERNEL_LINKS) {
        let Some(name) = link.get("Name").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let unit = link.clone();
        if let Ok(Some(adoption)) =
            tokio::task::spawn_blocking(move || adopt::adoption(&unit)).await
        {
            evidence.adoptions.insert(name.clone(), adoption);
        }
        let can = link.get("Type").and_then(Value::as_str) == Some("can");
        let wireguard = link.get("Kind").and_then(Value::as_str) == Some("wireguard");
        if can {
            let asked = name.clone();
            let value =
                match tokio::task::spawn_blocking(move || crate::netlink::can_link(&asked)).await {
                    Ok(Ok(Some(value))) => value,
                    Ok(Ok(None)) => absent("the kernel does not report this link as a CAN one"),
                    Ok(Err(err)) => absent(format!("the kernel could not be asked: {err}")),
                    Err(err) => absent(format!("the kernel could not be asked: {err}")),
                };
            evidence.can.insert(name, value);
        } else if wireguard {
            let asked = name.clone();
            let value = match tokio::task::spawn_blocking(move || {
                crate::netlink::wireguard_device(&asked)
            })
            .await
            {
                Ok(Ok(peers)) => json!({
                    "available": true,
                    "peers": peers.iter().map(crate::netlink::WireguardPeerObservation::to_json).collect::<Vec<_>>(),
                }),
                Ok(Err(err)) => absent(format!("the kernel could not be asked: {err}")),
                Err(err) => absent(format!("the kernel could not be asked: {err}")),
            };
            evidence.wireguard.insert(name, value);
        }
    }
    evidence
}

#[zbus::proxy(
    interface = "org.freedesktop.network1.Manager",
    default_service = "org.freedesktop.network1",
    default_path = "/org/freedesktop/network1"
)]
trait NetworkManager {
    fn describe(&self) -> zbus::Result<String>;
}

/// networkd's `Describe`, parsed and otherwise untouched.
pub(crate) async fn describe_raw() -> anyhow::Result<Value> {
    tokio::time::timeout(OBSERVE_TIMEOUT, async {
        let connection = zbus::Connection::system()
            .await
            .context("connect to the system bus for networkd")?;
        let proxy = NetworkManagerProxy::new(&connection)
            .await
            .context("connect to systemd-networkd")?;
        let json = proxy.describe().await.context("networkd Describe")?;
        serde_json::from_str(&json).context("parse networkd Describe")
    })
    .await
    .context("networkd observation timed out")?
}

#[async_trait::async_trait]
impl NetworkState for SystemdNetworkState {
    async fn describe(&self) -> anyhow::Result<Value> {
        normalize(describe_raw().await?)
    }

    async fn observe(&self) -> anyhow::Result<Value> {
        let describe = describe_raw().await.map_err(|err| format!("{err:#}"));
        let mut radios = radio_evidence(&self.root);
        if let Some(commands) = &self.commands {
            radios.wifi_capabilities =
                cached_capabilities(&self.capabilities, &self.root, commands.as_ref(), &radios)
                    .await;
        }
        let wifi = observe_wifi(&self.root, &radios.wifi_interfaces).await;
        let dns = if self.probe_dns {
            Some(observe_dns().await)
        } else {
            None
        };
        let describe = describe.as_ref().map_err(String::as_str);
        let links = observe_links(describe).await;
        Ok(observed_json(
            describe,
            &wifi,
            dns.as_ref(),
            &radios,
            &links,
        ))
    }
}

pub(crate) fn normalize(value: Value) -> anyhow::Result<Value> {
    let interfaces = value
        .get("Interfaces")
        .and_then(Value::as_array)
        .context("networkd Describe has no Interfaces array")?;
    let interfaces: Vec<Value> = interfaces.iter().filter_map(normalize_interface).collect();
    Ok(serde_json::json!({
        "interfaceCount": interfaces.len(),
        "interfaces": interfaces,
    }))
}

fn normalize_interface(source: &Value) -> Option<Value> {
    let source = source.as_object()?;
    let mut target = Map::new();
    for (from, to) in [
        ("Index", "index"),
        ("Name", "name"),
        ("Kind", "kind"),
        ("Type", "type"),
        ("Driver", "driver"),
        ("AdministrativeState", "administrativeState"),
        ("OperationalState", "operationalState"),
        ("CarrierState", "carrierState"),
        ("AddressState", "addressState"),
        ("IPv4AddressState", "ipv4AddressState"),
        ("IPv6AddressState", "ipv6AddressState"),
        ("OnlineState", "onlineState"),
        ("MTU", "mtu"),
        ("HardwareAddress", "hardwareAddress"),
        ("Addresses", "addresses"),
        ("DNS", "dns"),
        ("Routes", "routes"),
    ] {
        if let Some(value) = source.get(from) {
            target.insert(to.to_string(), value.clone());
        }
    }
    Some(Value::Object(target))
}

// ---- the observed surface -------------------------------------------------

#[cfg(test)]
mod tests;
