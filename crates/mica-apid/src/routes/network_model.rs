//! The network resources and the rules a network write is held to.

use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::IfaceSettings;
use serde_json::Value;

use super::*;

// These structs exist for `openapi.json` and are never deserialized
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
    /// Name of the `network` entry this VLAN sits on. A write naming a parent
    /// it does not declare declares it with the entry that keeps the image's
    /// configuration of that link, or is refused at 422.
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

/// The parameters of a CAN controller.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CanParameters {
    /// The nominal (arbitration) bit rate, 1 to 1 000 000 bit/s.
    pub(super) bitrate: u32,
    /// The delay before the controller restarts after bus-off, 0 to 60 000
    /// ms; 0 or absent never restarts it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) restart_ms: Option<u32>,
    /// CAN FD on or off; absent is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fd: Option<bool>,
    /// The CAN FD data-phase bit rate, 1 to 8 000 000 bit/s: required with
    /// `fd: true`, refused otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) data_bitrate: Option<u32>,
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
    /// A label for the peer, up to 64 characters with no control character.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    /// A pre-shared key: 32 bytes in padded base64. **Read as
    /// `<redacted>`**; a `PUT` of the interface or the map that sends
    /// `<redacted>` keeps the key stored for the peer with that public key.
    #[serde(rename = "presharedKey", skip_serializing_if = "Option::is_none")]
    pub(super) preshared_key: Option<String>,
}

/// One peer as the peer collection reads it: whether it has a pre-shared key
/// rather than the key, and room for what the tunnel reports about it.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireguardPeerState {
    /// The peer's X25519 public key, also its `DELETE` path segment.
    pub(super) public_key: String,
    /// A label for the peer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    /// Whether a pre-shared key is stored for the peer; the key is never read.
    pub(super) preshared_key: bool,
    /// `host:port` to send to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) endpoint: Option<String>,
    /// CIDRs routed to this peer.
    pub(super) allowed_ips: Vec<String>,
    /// Keepalive interval in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) persistent_keepalive: Option<u16>,
    /// The last handshake, Unix seconds, as the kernel reports it; `null`
    /// when the peer never completed one or the tunnel could not be asked.
    pub(super) latest_handshake: Option<u64>,
    /// Bytes received from the peer; `null` when the tunnel could not be
    /// asked.
    pub(super) rx_bytes: Option<u64>,
    /// Bytes sent to the peer; `null` when the tunnel could not be asked.
    pub(super) tx_bytes: Option<u64>,
}

impl From<&micad_settings::WireguardPeer> for WireguardPeerState {
    fn from(peer: &micad_settings::WireguardPeer) -> Self {
        Self {
            public_key: peer.public_key.clone(),
            name: peer.name.clone(),
            preshared_key: peer.preshared_key.is_some(),
            endpoint: peer.endpoint.clone(),
            allowed_ips: peer.allowed_ips.clone(),
            persistent_keepalive: peer.persistent_keepalive,
            latest_handshake: None,
            rx_bytes: None,
            tx_bytes: None,
        }
    }
}

/// Whether any peer in `entries` carries the redaction sentinel as its
/// pre-shared key.
pub(super) fn carries_redacted_psk<'a>(
    mut entries: impl Iterator<Item = &'a IfaceSettings>,
) -> bool {
    entries.any(|cfg| {
        cfg.wireguard.as_ref().is_some_and(|wireguard| {
            wireguard
                .peers
                .iter()
                .any(|peer| peer.preshared_key.as_deref() == Some(crate::redact::REDACTED))
        })
    })
}

/// `cfg` with each peer's `presharedKey` of `<redacted>` replaced by the key
/// stored for the peer with the same public key: a client that read the map
/// and sends it back means the key it could not see. A sentinel for a peer
/// with no stored key is refused, since there is no key it could mean.
pub(super) fn restore_preshared_keys(
    iface: &str,
    cfg: &mut IfaceSettings,
    stored: &NetworkEntries,
) -> Result<(), Box<Response>> {
    let Some(wireguard) = &mut cfg.wireguard else {
        return Ok(());
    };
    let stored_peers = stored
        .get(iface)
        .and_then(|entry| entry.wireguard.as_ref())
        .map_or(&[][..], |wireguard| wireguard.peers.as_slice());
    for (index, peer) in wireguard.peers.iter_mut().enumerate() {
        if peer.preshared_key.as_deref() != Some(crate::redact::REDACTED) {
            continue;
        }
        let kept = stored_peers
            .iter()
            .find(|stored| stored.public_key == peer.public_key)
            .and_then(|stored| stored.preshared_key.clone());
        let Some(kept) = kept else {
            return Err(Box::new(api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid(
                    "validation_failed",
                    format!(
                        "network.{iface} peer {index} sends `<redacted>` as its preshared key, and no key is stored for that peer to keep"
                    ),
                )
                .at(&iface_settings_path(iface)),
            )));
        };
        peer.preshared_key = Some(kept);
    }
    Ok(())
}

/// One `network` entry, as the document describes it.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct NetworkInterface {
    /// `physical`, `vlan`, `bridge`, `wireguard` or `can`. Absent means
    /// `physical`.
    ///
    /// The block below is authoritative and the interface name is not:
    /// `eth0.100` is a convention, not a declaration.
    pub(super) kind: String,
    /// Whether the interface acquires its address via DHCP; absent is false,
    /// and a CAN interface never does.
    pub(super) dhcp: bool,
    /// Static addressing, used when `dhcp` is false. Absent is an interface
    /// with no addressing at all, which is what a bridge port must be.
    #[serde(rename = "static", skip_serializing_if = "Option::is_none")]
    pub(super) static_: Option<StaticAddressing>,
    /// DNS servers a DHCP entry uses instead of the ones its lease gives.
    /// Only with `dhcp`; a static entry names its servers in `static.dns`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) dns: Vec<String>,
    /// VLAN parameters, for `kind = "vlan"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) vlan: Option<VlanParameters>,
    /// Bridge parameters, for `kind = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) bridge: Option<BridgeParameters>,
    /// WireGuard parameters, for `kind = "wireguard"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) wireguard: Option<WireguardParameters>,
    /// CAN parameters, for `kind = "can"`, which carries no addressing or
    /// routes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) can: Option<CanParameters>,
    /// Static routes this interface carries, beyond the default route
    /// `static.gateway` declares. Empty and absent are the same thing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) routes: Vec<StaticRoute>,
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

/// A write refused by `violations`, or nothing when there are none: 422 with
/// the first rule's code, message and values, and every rule broken under
/// `details`, each naming the entry and the field.
pub(super) fn violations_refusal(
    violations: Vec<NetworkViolation>,
    path: &str,
) -> Result<(), Box<Response>> {
    let Some(first) = violations.first() else {
        return Ok(());
    };
    let error = ApiError::apid(first.code, first.message.clone())
        .with_params(first.params.clone())
        .at(path);
    Err(Box::new(api_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        error.with_details(violations.into_iter().map(ErrorDetail::from).collect()),
    )))
}

/// Declare each VLAN parent `entries` names but does not declare, with the
/// entry that keeps what the image's own `.network` gives it (the device's
/// `adoption` of that link), and answer the parents declared. A parent whose
/// file holds what an entry cannot say is 422 `vlan_parent_unsupported`; one
/// the device offers nothing for is left to `vlan_parent_invalid`.
pub(super) async fn adopt_vlan_parents(
    state: &AppState,
    entries: &mut NetworkEntries,
    path: &str,
) -> Result<Vec<String>, Box<Response>> {
    let parents: std::collections::BTreeMap<String, String> = entries
        .iter()
        .filter_map(|(iface, cfg)| Some((cfg.vlan.as_ref()?.parent.clone(), iface.clone())))
        .filter(|(parent, _)| !entries.contains_key(parent))
        .collect();
    if parents.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(observed) = state.api.get_observed_network().await else {
        return Ok(Vec::new());
    };
    let links = observed
        .pointer("/interfaces/entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut adopted = Vec::new();
    let mut refused = Vec::new();
    for (parent, child) in parents {
        let Some(adoption) = links
            .iter()
            .find(|link| link.get("name").and_then(Value::as_str) == Some(parent.as_str()))
            .and_then(|link| link.get("adoption"))
        else {
            continue;
        };
        if let Some(entry) = adoption.get("entry")
            && let Ok(cfg) = serde_json::from_value::<IfaceSettings>(entry.clone())
        {
            entries.insert(parent.clone(), cfg);
            adopted.push(parent);
        } else if let Some(reason) = adoption.get("refused").and_then(Value::as_str) {
            refused.push(NetworkViolation::new(
                &child,
                "vlan.parent",
                "vlan_parent_unsupported",
                serde_json::json!({ "parent": parent, "reason": reason }),
                format!(
                    "the default configuration of {parent} cannot be taken over by mica ({reason}); configure {parent} first"
                ),
            ));
        }
    }
    violations_refusal(refused, path)?;
    Ok(adopted)
}

/// `entries` refused by a relational rule ([`relational_violations`]).
pub(super) fn relational_refusal(
    entries: &NetworkEntries,
    path: &str,
) -> Result<(), Box<Response>> {
    violations_refusal(relational_violations(entries), path)
}

/// A candidate map refused by any rule it breaks ([`map_violations`]); the
/// per-entry rules for the entries the request carries only, so bad data
/// already stored in an entry the request does not touch does not fail it.
pub(super) fn map_refusal(
    entries: &NetworkEntries,
    carried: &[&str],
    path: &str,
) -> Result<(), Box<Response>> {
    violations_refusal(map_violations(entries, carried), path)
}

/// A candidate map written back whole, at the `network` root. Answers the id
/// of the task that applies it.
pub(super) async fn write_network_map(
    state: &AppState,
    entries: &NetworkEntries,
) -> Result<String, Box<Response>> {
    // Infallible: the map's keys are strings and its values are structs of
    // scalars, strings and vectors.
    let value = encode(entries)?;
    state
        .api
        .set_settings(NETWORK_SETTINGS_PATH, &value)
        .await
        .map_err(|err| Box::new(bus_api_error(&err, Some(NETWORK_SETTINGS_PATH))))
}

/// The version of a stored network map: the HMAC-SHA256 of its JSON under a
/// key drawn when apid starts, in hex. Keyed because the map carries
/// pre-shared keys the read redacts, and a plain digest would let a reader
/// check a guess at one. `GET /api/v1/network` answers it, and a whole-map
/// `PUT` names it as `baseVersion`.
pub(super) fn network_version(map: &Value) -> String {
    static KEY: std::sync::OnceLock<aws_lc_rs::hmac::Key> = std::sync::OnceLock::new();
    let key = KEY.get_or_init(|| {
        aws_lc_rs::hmac::Key::new(
            aws_lc_rs::hmac::HMAC_SHA256,
            &micad_settings::random_bytes::<32>(),
        )
    });
    hex::encode(aws_lc_rs::hmac::sign(key, map.to_string().as_bytes()))
}

/// Refuse with 409 `busy` while a network task has not finished: one network
/// apply at a time. The tasks come from the signal mirror, or from micad
/// while the subscription is down; when micad cannot say, the write goes on
/// and meets the same micad.
pub(super) async fn check_network_idle(state: &AppState) -> Result<(), Box<Response>> {
    let busy = state.task_records().await.is_ok_and(|tasks| {
        tasks
            .iter()
            .any(|task| !task.terminal() && task.kind == "network.apply")
    });
    if !busy {
        return Ok(());
    }
    Err(Box::new(api_response(
        StatusCode::CONFLICT,
        ApiError::apid(
            "busy",
            "a network change is still being applied; try again when it finishes".to_string(),
        )
        .at(NETWORK_SETTINGS_PATH),
    )))
}

/// The whole-map write: the map and the version it was read at.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NetworkMapWrite {
    /// The `version` `GET /api/v1/network` answered; another stored version is
    /// 409 `version_conflict`.
    pub(super) base_version: String,
    /// The whole map that replaces the stored one.
    #[schema(value_type = std::collections::BTreeMap<String, NetworkInterface>)]
    pub(super) configured: NetworkEntries,
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkWriteQuery {
    /// Validate and answer what would change; write nothing.
    pub(super) dry_run: Option<bool>,
}

/// A whole-map write that was accepted.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkAccepted {
    /// Poll this id at `GET /api/v1/tasks/{id}`.
    pub(super) task_id: String,
    /// The stored map's version after the write.
    pub(super) version: String,
}

/// What a whole-map write would do.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkDryRun {
    /// VLAN parents the write would declare on the operator's behalf, each
    /// with the entry that keeps the image's configuration of that link.
    pub(super) adopted: Vec<String>,
    /// What the operator should know before writing; none.
    pub(super) warnings: Vec<String>,
    /// Each interface the write adds, changes or removes, by name.
    pub(super) diff: Vec<NetworkChange>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkChange {
    pub(super) interface: String,
    /// `added`, `modified` or `removed`.
    pub(super) change: &'static str,
}

/// The interfaces `next` adds, changes or removes against `stored`, by name.
pub(super) fn network_diff(stored: &NetworkEntries, next: &NetworkEntries) -> Vec<NetworkChange> {
    let as_value = |cfg: &IfaceSettings| serde_json::to_value(cfg).ok();
    let mut changes = Vec::new();
    for (name, cfg) in next {
        let change = match stored.get(name) {
            None => "added",
            Some(old) if as_value(old) != as_value(cfg) => "modified",
            Some(_) => continue,
        };
        changes.push(NetworkChange {
            interface: name.clone(),
            change,
        });
    }
    for name in stored.keys().filter(|name| !next.contains_key(*name)) {
        changes.push(NetworkChange {
            interface: name.clone(),
            change: "removed",
        });
    }
    changes
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkOverview {
    /// The declared settings map. This is desired configuration, not proof of
    /// link health.
    pub(super) configured: Value,
    pub(super) configured_count: usize,
    /// The stored map's version: name it as `baseVersion` on
    /// `PUT /api/v1/network` to replace only this map.
    pub(super) version: String,
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
