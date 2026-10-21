//! [`NetworkState`] on an OpenRC root, where no networkd describes the links.
//!
//! The same document networkd's `Describe` gives is assembled from what the
//! root has -- sysfs for each link, busybox `ip` for the addresses and the
//! routes, Base's `/run/mica/resolv.d/<iface>` for each link's DNS servers,
//! and the access point's udhcpd lease file for the addresses it handed out
//! -- so the one reduction ([`observed_json`]) serves both inits.
//! The radios are asked as on systemd: wpa_supplicant's and hostapd's control
//! sockets.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{Commands, Host};
use crate::network_state::{
    NET_CLASS_DIR, NetworkState, cached_capabilities, normalize, observe_links, observe_wifi,
    observed_json, radio_evidence,
};

/// Base's per-interface resolver files, relative to the root.
const RESOLVERS_DIR: &str = "run/mica/resolv.d";
/// The access point's service parameters micad writes, relative to the root:
/// `interface=` names the link udhcpd serves.
const AP_ENV: &str = "run/mica/wifi-ap.env";
/// The lease file the access point's udhcpd writes, relative to the root.
const AP_LEASES: &str = "run/mica/wifi-ap-udhcpd.leases";
/// One busybox `struct dyn_lease`: expiry, address, MAC, a 20-byte hostname
/// and two bytes of padding.
const UDHCPD_LEASE_BYTES: usize = 36;
/// What precedes the leases: the 64-bit time the file was written.
const UDHCPD_HEADER_BYTES: usize = 8;

pub struct IpNetworkState {
    root: PathBuf,
    commands: Arc<dyn Commands>,
    /// The radio's capabilities, asked once.
    capabilities: std::sync::OnceLock<Value>,
}

impl IpNetworkState {
    pub fn new(root: impl Into<PathBuf>, commands: Arc<dyn Commands>) -> Self {
        Self {
            root: root.into(),
            commands,
            capabilities: std::sync::OnceLock::new(),
        }
    }

    pub fn production() -> Self {
        Self::new("/", Arc::new(Host))
    }

    async fn ip(&self, args: &[&str]) -> anyhow::Result<String> {
        Ok(self.commands.run("ip", args).await?.success("ip")?.stdout)
    }

    /// The `Describe` document of this root's links.
    async fn describe_raw(&self) -> anyhow::Result<Value> {
        let addresses = parse_addresses(&self.ip(&["-o", "addr", "show"]).await?);
        let mut routes = parse_routes(&self.ip(&["-4", "route", "show"]).await?, 2);
        routes.extend(parse_routes(&self.ip(&["-6", "route", "show"]).await?, 10));
        let ap_interface = std::fs::read_to_string(self.root.join(AP_ENV))
            .ok()
            .and_then(|env| {
                env.lines()
                    .find_map(|line| line.strip_prefix("interface=").map(str::to_string))
            });
        let net = self.root.join(NET_CLASS_DIR);
        let mut names: Vec<String> = std::fs::read_dir(&net)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        let interfaces: Vec<Value> = names
            .iter()
            .map(|name| {
                let mut value = link(
                    &net.join(name),
                    name,
                    &addresses,
                    &routes,
                    &self.root.join(RESOLVERS_DIR).join(name),
                );
                if ap_interface.as_deref() == Some(name.as_str())
                    && let Ok(bytes) = std::fs::read(self.root.join(AP_LEASES))
                {
                    value["DHCPServer"] = json!({ "Leases": parse_udhcpd_leases(&bytes) });
                }
                value
            })
            .collect();
        Ok(json!({ "Interfaces": interfaces }))
    }
}

fn read(dir: &Path, attribute: &str) -> Option<String> {
    mica_fs::read_trimmed(&dir.join(attribute))
}

fn bytes(address: IpAddr) -> Value {
    match address {
        IpAddr::V4(v4) => json!(v4.octets()),
        IpAddr::V6(v6) => json!(v6.octets()),
    }
}

fn family(address: IpAddr) -> i64 {
    if address.is_ipv4() { 2 } else { 10 }
}

/// One address of `ip -o addr show`: the link, the address and its prefix.
struct Address {
    link: String,
    address: IpAddr,
    prefix: u8,
    scope_host: bool,
}

fn parse_addresses(text: &str) -> Vec<Address> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let _index = words.next()?;
            let link = words.next()?.trim_end_matches(':').to_string();
            let family = words.next()?;
            if family != "inet" && family != "inet6" {
                return None;
            }
            let (address, prefix) = words.next()?.split_once('/')?;
            let scope_host = line.contains(" scope host");
            Some(Address {
                link,
                address: address.parse().ok()?,
                prefix: prefix.parse().ok()?,
                scope_host,
            })
        })
        .collect()
}

/// One `<destination> [via <gateway>] dev <link> [metric <n>]` route of the
/// main table; `default` is the zero destination.
struct Route {
    link: String,
    family: i64,
    destination: Option<IpAddr>,
    prefix: u8,
    gateway: Option<IpAddr>,
    metric: Option<u64>,
}

/// The routes `ip route show` lists. A line whose first word is a route type
/// (`unreachable`, `blackhole`) rather than a destination is left out: it
/// leaves by no link.
fn parse_routes(text: &str, family: i64) -> Vec<Route> {
    text.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let (destination, prefix) = match *words.first()? {
                "default" => (None, 0),
                word => {
                    let (address, prefix) = word.split_once('/').unwrap_or((word, ""));
                    let address: IpAddr = address.parse().ok()?;
                    let full = if address.is_ipv4() { 32 } else { 128 };
                    let prefix = if prefix.is_empty() {
                        full
                    } else {
                        prefix.parse().ok()?
                    };
                    (Some(address), prefix)
                }
            };
            let after = |key: &str| {
                words
                    .iter()
                    .position(|word| *word == key)
                    .and_then(|at| words.get(at + 1).copied())
            };
            Some(Route {
                link: after("dev")?.to_string(),
                family,
                destination,
                prefix,
                gateway: after("via").and_then(|gateway| gateway.parse().ok()),
                metric: after("metric").and_then(|metric| metric.parse().ok()),
            })
        })
        .collect()
}

/// The leases in a busybox udhcpd lease file, as networkd's DHCP server
/// reports its own: `Address`, `Hostname`, `HardwareAddress`.
fn parse_udhcpd_leases(bytes: &[u8]) -> Vec<Value> {
    bytes
        .get(UDHCPD_HEADER_BYTES..)
        .unwrap_or_default()
        .as_chunks::<UDHCPD_LEASE_BYTES>()
        .0
        .iter()
        .map(|lease| {
            let hostname = &lease[14..34];
            let hostname = hostname.split(|byte| *byte == 0).next().unwrap_or_default();
            let mut value = json!({
                "Address": &lease[4..8],
                "HardwareAddress": &lease[8..14],
            });
            if let Ok(hostname) = std::str::from_utf8(hostname)
                && !hostname.is_empty()
            {
                value["Hostname"] = json!(hostname);
            }
            value
        })
        .collect()
}

fn link(
    dir: &Path,
    name: &str,
    addresses: &[Address],
    routes: &[Route],
    resolvers: &Path,
) -> Value {
    let link_type = read(dir, "type");
    let loopback = link_type.as_deref() == Some("772");
    let can = link_type.as_deref() == Some("280");
    let carrier = read(dir, "carrier").as_deref() == Some("1");
    let up = read(dir, "operstate").as_deref() == Some("up") || (loopback && carrier);
    let own: Vec<&Address> = addresses.iter().filter(|a| a.link == name).collect();
    let routable = own.iter().any(|address| !address.scope_host);
    let operational = match (up, carrier, routable) {
        _ if loopback => "carrier",
        (true, true, true) => "routable",
        (true, true, false) => "carrier",
        (_, false, _) | (false, _, _) => "no-carrier",
    };
    let hardware: Option<Vec<u8>> = read(dir, "address").and_then(|mac| {
        mac.split(':')
            .map(|byte| u8::from_str_radix(byte, 16).ok())
            .collect()
    });
    let dns: Vec<Value> = std::fs::read_to_string(resolvers)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("nameserver "))
        .filter_map(|server| server.trim().parse::<IpAddr>().ok())
        .map(|server| json!({ "Family": family(server), "Address": bytes(server) }))
        .collect();
    let mut value = json!({
        "Index": read(dir, "ifindex").and_then(|index| index.parse::<u64>().ok()),
        "Name": name,
        "Type": if loopback { "loopback" } else if can { "can" } else { "ether" },
        "OperationalState": operational,
        "CarrierState": if carrier { "carrier" } else { "no-carrier" },
        "MTU": read(dir, "mtu").and_then(|mtu| mtu.parse::<u64>().ok()),
        "Addresses": own.iter().map(|address| json!({
            "Family": family(address.address),
            "Address": bytes(address.address),
            "PrefixLength": address.prefix,
        })).collect::<Vec<_>>(),
        "Routes": routes.iter().filter(|route| route.link == name).map(|route| {
            let mut entry = json!({
                "Family": route.family,
                "Destination": match route.destination {
                    Some(destination) => bytes(destination),
                    None if route.family == 2 => json!([0, 0, 0, 0]),
                    None => json!(vec![0_u8; 16]),
                },
                "DestinationPrefixLength": route.prefix,
            });
            if let Some(gateway) = route.gateway {
                entry["Gateway"] = bytes(gateway);
            }
            if let Some(metric) = route.metric {
                entry["Priority"] = json!(metric);
            }
            entry
        }).collect::<Vec<_>>(),
        "DNS": dns,
    });
    if let Some(hardware) = hardware.filter(|bytes| bytes.len() == 6 && !loopback) {
        value["HardwareAddress"] = json!(hardware);
    }
    value
}

#[async_trait::async_trait]
impl NetworkState for IpNetworkState {
    async fn describe(&self) -> anyhow::Result<Value> {
        normalize(self.describe_raw().await?)
    }

    async fn observe(&self) -> anyhow::Result<Value> {
        let describe = self.describe_raw().await.map_err(|err| format!("{err:#}"));
        let mut radios = radio_evidence(&self.root);
        radios.wifi_capabilities = cached_capabilities(
            &self.capabilities,
            &self.root,
            self.commands.as_ref(),
            &radios,
        )
        .await;
        let wifi = observe_wifi(&self.root, &radios.wifi_interfaces).await;
        let describe = describe.as_ref().map_err(String::as_str);
        let links = observe_links(describe).await;
        Ok(observed_json(describe, &wifi, None, &radios, &links))
    }
}

#[cfg(test)]
mod tests;
