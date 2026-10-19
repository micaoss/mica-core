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

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Map, Value};

mod dns;
mod json;
mod radio;
mod wifi;
pub use dns::*;
pub use json::*;
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
}

impl SystemdNetworkState {
    /// The observer `main.rs` attaches on a device: rooted at `/`, DNS
    /// probe on.
    #[must_use]
    pub fn production() -> Self {
        Self::at("/").with_dns_probe(true)
    }

    /// An observer rooted at `root`, with NO DNS probe: nothing here touches
    /// the network until the probe is switched on, so a fixture tree cannot
    /// make a test resolve a name against the machine running it.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            probe_dns: false,
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

#[zbus::proxy(
    interface = "org.freedesktop.network1.Manager",
    default_service = "org.freedesktop.network1",
    default_path = "/org/freedesktop/network1"
)]
trait NetworkManager {
    fn describe(&self) -> zbus::Result<String>;
}

/// networkd's `Describe`, parsed and otherwise untouched.
async fn describe_raw() -> anyhow::Result<Value> {
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
        let radios = radio_evidence(&self.root);
        let wifi = observe_wifi(&self.root, &radios.wifi_interfaces).await;
        let dns = if self.probe_dns {
            Some(observe_dns().await)
        } else {
            None
        };
        Ok(observed_json(
            describe.as_ref().map_err(String::as_str),
            &wifi,
            dns.as_ref(),
            &radios,
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
