//! The network resources and the rules a network write is held to.

use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::IfaceSettings;
use serde_json::Value;
use std::net::IpAddr;

use super::*;

// These four structs exist for `openapi.json` and are never deserialized
// from: the routes parse into `micad_settings`' own types, which are the
// validator micad runs. A test holds each one against the model field for
// field.
/// `static` addressing.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct StaticAddressing {
    /// Interface address in CIDR notation, e.g. `192.168.1.10/24`.
    pub(super) address: String,
    /// Default gateway address; absent for a link with no route of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) gateway: Option<String>,
    /// DNS server addresses.
    pub(super) dns: Vec<String>,
}

/// The 802.1Q parameters of a VLAN interface.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct VlanParameters {
    /// Name of the `network` entry this VLAN sits on. **It must be a declared
    /// entry**; a body naming one that is not is refused at 422.
    pub(super) parent: String,
    /// 802.1Q VLAN id.
    pub(super) id: u16,
}

/// The parameters of a software bridge.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct BridgeParameters {
    /// Names of the `network` entries enslaved to this bridge. **Each must be
    /// a declared entry, must carry no addressing of its own, and must not be
    /// a port of another bridge**; a body breaking any of those is refused at
    /// 422 with the rule's own sentence.
    pub(super) ports: Vec<String>,
}

/// The parameters of a WireGuard tunnel.
///
/// There is no private-key member and there never will be: this subtree is
/// served over `GET /api/v1/settings/network`, so a key in it is a key
/// published to every client. The private key lives in a mode-0640 file on the
/// device and only its public half is surfaced, through
/// `GET /api/v1/state/network` and the rotate action.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct WireguardParameters {
    /// UDP port to listen on; absent lets the kernel pick one.
    #[serde(rename = "listenPort", skip_serializing_if = "Option::is_none")]
    pub(super) listen_port: Option<u16>,
    /// The far ends of the tunnel. A `PUT` of this interface replaces them;
    /// the peer collection route edits them one at a time.
    pub(super) peers: Vec<WireguardPeerEntry>,
}

/// One far end of a WireGuard tunnel, in both directions.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct WireguardPeerEntry {
    /// The peer's X25519 public key: 32 bytes in padded base64. It is also
    /// this entry's `DELETE` path segment, and its alphabet contains `/`, so a
    /// key carrying one is percent-encoded there.
    #[serde(rename = "publicKey")]
    pub(super) public_key: String,
    /// CIDRs routed to this peer.
    #[serde(rename = "allowedIps")]
    pub(super) allowed_ips: Vec<String>,
    /// `host:port` to send to, for a peer this end initiates to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) endpoint: Option<String>,
    /// Keepalive interval in seconds, for a peer behind NAT.
    #[serde(
        rename = "persistentKeepalive",
        skip_serializing_if = "Option::is_none"
    )]
    pub(super) persistent_keepalive: Option<u16>,
}

/// One `network` entry, as the document describes it.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct NetworkInterface {
    /// `physical`, `vlan`, `bridge` or `wireguard`. Absent means `physical`.
    ///
    /// The block below is authoritative and the interface name is not:
    /// `eth0.100` is a convention, not a declaration.
    pub(super) kind: String,
    /// Whether the interface acquires its address via DHCP.
    pub(super) dhcp: bool,
    /// Static addressing, used when `dhcp` is false. Absent is an interface
    /// with no addressing at all, which is what a bridge port must be.
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub(super) static_: Option<StaticAddressing>,
    /// VLAN parameters, for `kind = "vlan"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) vlan: Option<VlanParameters>,
    /// Bridge parameters, for `kind = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) bridge: Option<BridgeParameters>,
    /// WireGuard parameters, for `kind = "wireguard"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) wireguard: Option<WireguardParameters>,
    /// Static routes this interface carries, beyond the default route
    /// `static.gateway` declares. Empty and absent are the same thing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) routes: Vec<StaticRoute>,
    /// The DHCP server this interface offers. Absent offers none, and a link
    /// with no static address of its own may not offer one.
    #[serde(rename = "dhcpServer", skip_serializing_if = "Option::is_none")]
    pub(super) dhcp_server: Option<DhcpServer>,
}

/// One static route.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StaticRoute {
    /// Where the route leads, in CIDR notation.
    pub(super) destination: String,
    /// The next hop; absent is an on-link route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) gateway: Option<String>,
    /// Route metric; absent leaves networkd's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) metric: Option<u32>,
}

/// The DHCP server offered on one interface.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DhcpServer {
    /// First address handed out, as an offset into the interface's subnet.
    pub(super) pool_offset: u32,
    /// How many addresses the pool holds; at least one.
    pub(super) pool_size: u32,
    /// DNS servers announced to clients; empty announces none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) dns: Vec<String>,
    /// Default lease time in seconds; absent leaves networkd's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) lease_seconds: Option<u32>,
}

/// The `network` subtree as typed entries, or the error envelope for whatever
/// prevented reading it.
///
/// **Strict, unlike [`parse_network`]**, which the pane uses: an entry whose
/// body does not parse is an error here and never a silently dropped entry.
/// The pane can afford to skip one and name it in the page, because a human is
/// reading the result; these routes cannot. Two of them rewrite the whole map,
/// so a dropped entry is a deleted interface, and all of them validate
/// relationally, so an invisible entry turns a legal bridge port into a 422.
/// It is the posture [`api_stored_keys`] and [`stored_networks`] already take:
/// an unreadable list is an error and never an empty one.
pub(super) async fn api_network_entries(state: &AppState) -> Result<NetworkEntries, Box<Response>> {
    let network = match state.api.get_settings(NETWORK_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return Err(Box::new(bus_api_error(&err, Some(NETWORK_SETTINGS_PATH)))),
    };
    let (entries, unreadable) = parse_network(&network);
    if !unreadable.is_empty() {
        return Err(Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!(
                    "the stored network map holds {} entr{} this build cannot read: {}. Every route under `/api/v1/network` validates the whole map, so none of them can act while part of it is unreadable",
                    unreadable.len(),
                    if unreadable.len() == 1 { "y" } else { "ies" },
                    unreadable.join(", ")
                ),
            )
            .at(NETWORK_SETTINGS_PATH),
        )));
    }
    Ok(entries)
}

/// `iface` checked as an interface name, or the 422 that says it is not one.
///
/// Section 2.4's malformed half, on every route of this cluster. The predicate
/// is [`valid_iface_name`], the one the pane and the setup wizard already use,
/// rather than a second spelling of the same charset.
pub(super) fn check_iface_name(iface: &str, path: &str) -> Result<(), Box<Response>> {
    if valid_iface_name(iface) {
        return Ok(());
    }
    Err(Box::new(api_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::apid(
            "validation_failed",
            "an interface name is 1 to 15 characters of letters, digits, `.`, `_`, `-` or `:`, and \
             not `.` or `..`"
                .to_string(),
        )
        .at(path),
    )))
}

/// `entries` refused by a relational rule, in the error envelope.
///
/// The message is [`validate_entries`]' own, which is the reconciler's own
/// sentence: no phrasing this route could pre-write would say which entry and
/// which field made the tree illegal.
pub(super) fn relational_refusal(
    entries: &NetworkEntries,
    path: &str,
) -> Result<(), Box<Response>> {
    validate_entries(entries).map_err(|message| {
        Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(path),
        ))
    })
}

/// One entry's static address refused by the wizard's rule, in the error
/// envelope.
///
/// The message is [`validate_static_address`]'s own -- the sentence the setup
/// wizard and the network pane already print -- so the typed routes and the
/// forms cannot state the rule in two ways.
///
/// The `path` member names the entry and not the subtree the route writes: on
/// `PUT /api/v1/network` that subtree is `network`, which would not say which
/// of the entries the body carried is the wrong one.
///
/// Called on the entries a **request** carries and never on the tree as read.
/// [`api_v1_network_iface_remove`] re-validates the stored map without the
/// removed entry, and putting this rule in that shared re-validation would make
/// removing an unrelated interface start failing on bad data already on disk.
/// The routing and DHCP-server rules of one entry.
///
/// Three refusals, all of them about configurations networkd would accept and
/// nothing could use:
///
/// - a route whose destination is not a network, or whose next hop is not an
///   address;
/// - a second default route, declared as a `0.0.0.0/0` route beside a
///   `static.gateway` that already is one;
/// - a DHCP server on a link with no address of its own, which has no subnet
///   to hand addresses out of.
pub(super) fn routing_refusal(iface: &str, cfg: &IfaceSettings) -> Result<(), Box<Response>> {
    let refuse = |message: String| {
        Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(&iface_settings_path(iface)),
        ))
    };
    let has_gateway = cfg
        .static_
        .as_ref()
        .is_some_and(|static_| static_.gateway.is_some());
    for route in &cfg.routes {
        if !is_ip_or_cidr(&route.destination) {
            return Err(refuse(format!(
                "the route destination {:?} is not a network in CIDR notation, such as `10.20.0.0/16`",
                route.destination
            )));
        }
        if let Some(gateway) = &route.gateway
            && gateway.parse::<IpAddr>().is_err()
        {
            return Err(refuse(format!(
                "the route via {gateway:?} does not name an address; omit the gateway for an on-link route"
            )));
        }
        if is_default_destination(&route.destination) && has_gateway {
            return Err(refuse(format!(
                "this entry declares a default route twice: once as `static.gateway` and once as the route to {:?}. Keep one",
                route.destination
            )));
        }
    }
    if cfg.dhcp_server.is_some() {
        let addressed = !cfg.dhcp && cfg.static_.is_some();
        if !addressed {
            return Err(refuse(
                "a DHCP server needs a static address on this interface: the pool is an offset into the interface's own subnet, and a link with no address has no subnet"
                    .to_string(),
            ));
        }
        if let Some(server) = &cfg.dhcp_server {
            if server.pool_size == 0 {
                return Err(refuse(
                    "the DHCP pool holds no addresses; give `poolSize` a count of at least 1"
                        .to_string(),
                ));
            }
            for dns in &server.dns {
                if dns.parse::<IpAddr>().is_err() {
                    return Err(refuse(format!(
                        "the DNS server {dns:?} announced to clients is not an address"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Whether a destination is the default route, in either family.
pub(super) fn is_default_destination(destination: &str) -> bool {
    matches!(destination, "0.0.0.0/0" | "::/0")
}

pub(super) fn address_refusal(iface: &str, cfg: &IfaceSettings) -> Result<(), Box<Response>> {
    let address = cfg
        .static_
        .as_ref()
        .map_or("", |static_| static_.address.as_str());
    validate_static_address(cfg.dhcp, address).map_err(|message| {
        Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message.to_string())
                .at(&iface_settings_path(iface)),
        ))
    })
}

/// A candidate map written back whole, at the `network` root.
pub(super) async fn write_network_map(
    state: &AppState,
    entries: &NetworkEntries,
) -> Result<(), Box<Response>> {
    // Infallible: the map's keys are strings and its values are structs of
    // scalars, strings and vectors.
    let value = encode(entries)?;
    if let Err(err) = state.api.set_settings(NETWORK_SETTINGS_PATH, &value).await {
        return Err(Box::new(bus_api_error(&err, Some(NETWORK_SETTINGS_PATH))));
    }
    Ok(())
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkOverview {
    /// The declared settings map. This is desired configuration, not proof of
    /// link health.
    pub(super) configured: Value,
    pub(super) configured_count: usize,
    /// The current view reported by systemd-networkd.
    pub(super) observed: ObservedNetwork,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ObservedNetwork {
    pub(super) available: bool,
    pub(super) interface_count: usize,
    pub(super) interfaces: Vec<ObservedInterface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<&'static str>,
}

#[derive(serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ObservedInterface {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) driver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) administrative_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) operational_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) carrier_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) address_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ipv4_address_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ipv6_address_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) online_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mtu: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) hardware_address: Option<Value>,
    #[serde(default)]
    pub(super) addresses: Vec<Value>,
    #[serde(default)]
    pub(super) dns: Vec<Value>,
    #[serde(default)]
    pub(super) routes: Vec<Value>,
}

/// The `network` settings subtree, as a map of typed entries.
///
/// Parsed into `micad_settings` types rather than read out of the JSON by key,
/// so the pane renders exactly the schema micad deserializes and a field this
/// file misspells is a compile error rather than a blank input.
pub(super) type NetworkEntries = std::collections::BTreeMap<String, IfaceSettings>;

/// Parse readable entries from the configured network map.
pub(super) fn parse_network(network: &Value) -> (NetworkEntries, Vec<String>) {
    let empty = serde_json::Map::new();
    let mut entries = NetworkEntries::new();
    let mut unreadable = Vec::new();
    for (name, body) in network.as_object().unwrap_or(&empty) {
        match serde_json::from_value::<IfaceSettings>(body.clone()) {
            Ok(cfg) => {
                entries.insert(name.clone(), cfg);
            }
            Err(_) => unreadable.push(name.clone()),
        }
    }
    (entries, unreadable)
}

pub(super) fn peers_settings_path(iface: &str) -> String {
    format!("{}.wireguard.peers", iface_settings_path(iface))
}

// Power pane
