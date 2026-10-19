//! [`NetworkState`] on an OpenRC root, where no networkd describes the links.
//!
//! The same document networkd's `Describe` gives is assembled from what the
//! root has -- sysfs for each link, busybox `ip` for the addresses and the
//! default routes, and Base's `/run/mica/resolv.d/<iface>` for each link's DNS
//! servers -- so the one reduction ([`observed_json`]) serves both inits.
//! The radios are asked as on systemd: wpa_supplicant's and hostapd's control
//! sockets.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{Commands, Host};
use crate::network_state::{
    NET_CLASS_DIR, NetworkState, normalize, observe_wifi, observed_json, radio_evidence,
};

/// Base's per-interface resolver files, relative to the root.
const RESOLVERS_DIR: &str = "run/mica/resolv.d";

pub struct IpNetworkState {
    root: PathBuf,
    commands: Arc<dyn Commands>,
}

impl IpNetworkState {
    pub fn new(root: impl Into<PathBuf>, commands: Arc<dyn Commands>) -> Self {
        Self {
            root: root.into(),
            commands,
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
        let mut routes =
            parse_default_routes(&self.ip(&["-4", "route", "show", "default"]).await?, 2);
        routes.extend(parse_default_routes(
            &self.ip(&["-6", "route", "show", "default"]).await?,
            10,
        ));
        let net = self.root.join(NET_CLASS_DIR);
        let mut names: Vec<String> = std::fs::read_dir(&net)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        let interfaces: Vec<Value> = names
            .iter()
            .map(|name| {
                link(
                    &net.join(name),
                    name,
                    &addresses,
                    &routes,
                    &self.root.join(RESOLVERS_DIR).join(name),
                )
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

/// One `default via <gateway> dev <link> [metric <n>]` route.
struct Route {
    link: String,
    family: i64,
    gateway: Option<IpAddr>,
    metric: Option<u64>,
}

fn parse_default_routes(text: &str, family: i64) -> Vec<Route> {
    text.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            if words.first() != Some(&"default") {
                return None;
            }
            let after = |key: &str| {
                words
                    .iter()
                    .position(|word| *word == key)
                    .and_then(|at| words.get(at + 1).copied())
            };
            Some(Route {
                link: after("dev")?.to_string(),
                family,
                gateway: after("via").and_then(|gateway| gateway.parse().ok()),
                metric: after("metric").and_then(|metric| metric.parse().ok()),
            })
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
    let loopback = read(dir, "type").as_deref() == Some("772");
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
        "Type": if loopback { "loopback" } else { "ether" },
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
                "Destination": if route.family == 2 { json!([0, 0, 0, 0]) } else { json!(vec![0_u8; 16]) },
                "DestinationPrefixLength": 0,
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
        let radios = radio_evidence(&self.root);
        let wifi = observe_wifi(&self.root, &radios.wifi_interfaces).await;
        Ok(observed_json(
            describe.as_ref().map_err(String::as_str),
            &wifi,
            None,
            &radios,
        ))
    }
}

#[cfg(test)]
mod tests;
