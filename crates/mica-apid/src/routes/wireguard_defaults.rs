//! The values a new WireGuard tunnel starts from, and the UDP ports the
//! services on the device hold.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::NetworkViolation;
use serde_json::{Value, json};

use super::*;

/// The listen ports a new tunnel is offered, in order.
const PORTS: std::ops::RangeInclusive<u16> = 51821..=51899;

/// The values a new tunnel starts from: the first of each that nothing on the
/// device has. A value with no free candidate is absent.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireguardDefaults {
    /// `wg0`, `wg1`, … : the first no declared or observed link has.
    pub(super) name: Option<String>,
    /// `10.2.0.1/24` … `10.254.0.1/24`, then `172.16.100.1/24` …
    /// `172.16.255.1/24`: the first whose subnet overlaps no declared or
    /// observed address, no peer's allowed IPs and no observed route other
    /// than a default one.
    pub(super) address: Option<String>,
    /// 51821 … 51899: the first no tunnel declares and no service holds.
    pub(super) listen_port: Option<u16>,
    /// The UDP ports services on the device hold, for a client choosing past
    /// changes the device has not seen yet.
    pub(super) udp_ports_in_use: Vec<u16>,
}

/// The UDP ports userspace sockets hold, from `udp` and `udp6` under `dir`.
/// A kernel socket (inode 0), such as a WireGuard device's own, is not one:
/// the tunnel holding it is in the map.
pub(super) fn held_udp_ports(dir: &std::path::Path) -> BTreeSet<u16> {
    ["udp", "udp6"]
        .iter()
        .filter_map(|table| std::fs::read_to_string(dir.join(table)).ok())
        .flat_map(|text| {
            text.lines()
                .skip(1)
                .filter_map(|line| {
                    let columns: Vec<&str> = line.split_whitespace().collect();
                    if *columns.get(9)? == "0" {
                        return None;
                    }
                    u16::from_str_radix(columns.get(1)?.rsplit_once(':')?.1, 16).ok()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A tunnel whose listen port is new or changed and that a service on the
/// device holds is `port_in_use`.
pub(super) fn listener_violations(
    entries: &NetworkEntries,
    stored: &NetworkEntries,
    held: &BTreeSet<u16>,
) -> Vec<NetworkViolation> {
    entries
        .iter()
        .filter_map(|(iface, cfg)| {
            let port = cfg.wireguard.as_ref()?.listen_port?;
            let before = stored
                .get(iface)
                .and_then(|stored| stored.wireguard.as_ref()?.listen_port);
            (before != Some(port) && held.contains(&port)).then(|| {
                NetworkViolation::new(
                    iface,
                    "wireguard.listenPort",
                    "port_in_use",
                    json!({ "port": port }),
                    format!("a service on the device holds UDP port {port}"),
                )
            })
        })
        .collect()
}

/// `a.b.c.d/p` as the network and the prefix.
fn subnet(cidr: &str) -> Option<(u32, u8)> {
    let (address, prefix) = cidr.split_once('/')?;
    subnet_of(address, prefix.parse().ok()?)
}

fn subnet_of(address: &str, prefix: u8) -> Option<(u32, u8)> {
    let address: Ipv4Addr = address.parse().ok()?;
    (prefix <= 32).then(|| (u32::from(address) & mask(prefix), prefix))
}

fn mask(prefix: u8) -> u32 {
    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
}

fn overlaps((a, a_prefix): (u32, u8), (b, b_prefix): (u32, u8)) -> bool {
    let shorter = mask(a_prefix.min(b_prefix));
    a & shorter == b & shorter
}

/// The defaults over the stored map, the observation and the held ports.
fn defaults(entries: &NetworkEntries, observed: &Value, held: &BTreeSet<u16>) -> WireguardDefaults {
    let links: Vec<&Value> = observed["interfaces"]["entries"]
        .as_array()
        .map(|links| links.iter().collect())
        .unwrap_or_default();
    let names: BTreeSet<&str> = entries
        .keys()
        .map(String::as_str)
        .chain(links.iter().filter_map(|link| link["name"].as_str()))
        .collect();
    let mut taken: Vec<(u32, u8)> = Vec::new();
    for cfg in entries.values() {
        taken.extend(cfg.static_.as_ref().and_then(|cfg| subnet(&cfg.address)));
        for peer in cfg.wireguard.iter().flat_map(|cfg| &cfg.peers) {
            taken.extend(peer.allowed_ips.iter().filter_map(|cidr| subnet(cidr)));
        }
    }
    for link in &links {
        for address in link["addresses"].as_array().into_iter().flatten() {
            let prefix = address["prefixLength"]
                .as_u64()
                .and_then(|prefix| u8::try_from(prefix).ok());
            taken.extend(
                address["address"]
                    .as_str()
                    .zip(prefix)
                    .and_then(|(address, prefix)| subnet_of(address, prefix)),
            );
        }
        for route in link["routes"].as_array().into_iter().flatten() {
            taken.extend(
                route["dest"]
                    .as_str()
                    .and_then(subnet)
                    .filter(|(_, prefix)| *prefix > 0),
            );
        }
    }
    let ports: BTreeSet<u16> = entries
        .values()
        .filter_map(|cfg| cfg.wireguard.as_ref()?.listen_port)
        .chain(held.iter().copied())
        .collect();
    let candidates = (2..=254u8)
        .map(|n| Ipv4Addr::new(10, n, 0, 1))
        .chain((100..=255u8).map(|n| Ipv4Addr::new(172, 16, n, 1)));
    WireguardDefaults {
        name: (0..1000)
            .map(|n| format!("wg{n}"))
            .find(|name| !names.contains(name.as_str())),
        address: candidates
            .map(|address| (address, (u32::from(address) & mask(24), 24)))
            .find(|(_, candidate)| !taken.iter().any(|other| overlaps(*candidate, *other)))
            .map(|(address, _)| format!("{address}/24")),
        listen_port: PORTS.clone().find(|port| !ports.contains(port)),
        udp_ports_in_use: held.iter().copied().collect(),
    }
}

/// The values a new WireGuard tunnel starts from.
///
/// Computed on the device, so the listen port skips the UDP ports its
/// services hold, which no client can see.
#[utoipa::path(
    get,
    path = V1_NETWORK_WIREGUARD_DEFAULTS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The first free name, subnet and listen port, and the UDP ports services hold", body = WireguardDefaults),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_wireguard_defaults(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let entries = match api_network_entries(&state).await {
        Ok(entries) => entries,
        Err(response) => return *response,
    };
    let observed = state.api.get_observed_network().await.unwrap_or_default();
    let held = held_udp_ports(&state.proc_net);
    api_response(StatusCode::OK, defaults(&entries, &observed, &held))
}
