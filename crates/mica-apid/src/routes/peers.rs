//! WireGuard peers and key rotation.

use crate::redact;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::{IfaceKind, WireguardPeer};
use serde_json::Value;

use super::*;

/// The body of a successful key rotation: the public half, and nothing else.
///
/// There is no `privateKey` member here and there will not be one. The private
/// half never leaves micad — there is no read-back route for the private key,
/// ever; not redacted-on-read, nonexistent — so this struct is the whole of
/// what a rotation can answer.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireguardRotation {
    /// The new base64 X25519 public key, which is what the far end needs.
    pub(super) public_key: String,
}

// `iface` goes to micad unexamined: micad owns the "declared entry of kind
// wireguard" rule and raises a distinct error name for each half of it. A
// second copy of that rule here could disagree with the first.
/// Rotate a WireGuard interface's private key.
///
/// An action rather than a settings write: the private key lives in a
/// mode-0640 file on STATE that the settings tree does not describe. micad
/// draws a new key, tears down the device holding the old one and reconciles.
///
/// Answers **200** with the new public key — the tunnel is already running on
/// it. The private half is never returned. **404** when `iface` names no
/// declared `network` entry; **422** when the entry exists but is not of kind
/// `wireguard`.
#[utoipa::path(
    post,
    path = V1_WIREGUARD_ROTATE_ROUTE,
    context_path = API,
    tag = "actions",
    params(("iface" = String, Path, description = "The `network` entry to rotate, which must be one of kind `wireguard`: `wg0`")),
    responses(
        (status = 200, description = "A new key was drawn; the body carries its public half", body = WireguardRotation),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The name is not a declared `network` entry (`settings_not_found`); the URL names no interface to rotate", body = ApiError),
        (status = 422, description = "The entry exists and is not a WireGuard one (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to rotate (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wireguard_rotate(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(iface): Path<String>,
) -> Response {
    match state.api.rotate_wireguard_key(&iface).await {
        Ok(public_key) => api_response(StatusCode::OK, WireguardRotation { public_key }),
        // The `path` is the settings dot-path at fault, and this failure has
        // one: the entry whose kind micad refused.
        Err(err) => bus_api_error(&err, Some(&iface_settings_path(&iface))),
    }
}

// The token collection: the listing, the mint and the revocation.

/// A tunnel's stored peers, or the error envelope for whatever refuses the interface.
///
/// The three answers this cluster gives about an `{iface}` that is not a
/// usable tunnel, and none of them is interchangeable with another:
///
/// - **404** when no `network` entry has that name. This is what the pane gets
///   wrong: `POST /network/peers/add` on an undeclared interface *succeeds* there and
///   writes an entry of the default kind carrying a WireGuard block, because
///   the pane's `stored_peers` answers an empty list rather than an error and
///   the settings setter creates missing intermediates by documented contract.
///   `the_pane_peer_add_writes_a_broken_entry_for_an_undeclared_interface`
///   runs that and confirms it. Here the read is the guard, and it happens
///   before anything is written.
/// - **422** when the entry exists and is not of kind `wireguard`. The same
///   split micad's rotate-key makes: a well-formed identifier naming a real
///   entry of the wrong kind is a bad argument and not an absent resource.
/// - **422** when the name is not an interface name at all, from
///   [`check_iface_name`].
pub(super) async fn tunnel_peers(
    state: &AppState,
    iface: &str,
    path: &str,
) -> Result<Vec<WireguardPeer>, Box<Response>> {
    check_iface_name(iface, path)?;
    let entries = api_network_entries(state).await?;
    let Some(cfg) = entries.get(iface) else {
        return Err(Box::new(item_not_found(NETWORK_SETTINGS_PATH, iface)));
    };
    if cfg.kind != IfaceKind::Wireguard {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "network.{iface} is not a WireGuard interface: only an entry of kind `wireguard` has peers"
                ),
            )
            .at(path),
        )));
    }
    Ok(cfg
        .wireguard
        .as_ref()
        .map(|wireguard| wireguard.peers.clone())
        .unwrap_or_default())
}

/// Validate and write a rewritten peer list, in the error envelope.
///
/// The API's own writer and not [`write_peers`], which answers a re-rendered
/// pane at 422 and a redirect on success. The **validator** is the same one:
/// [`validate_peers`] echoes the rules `validate_wireguard` runs in the
/// reconciler, and it names a rejected peer by its index and never by its key,
/// for the reason the reconciler states -- an operator who pasted a *private*
/// key into the field would otherwise find it in the error text, and here that
/// text goes into an HTTP body.
pub(super) async fn api_write_peers(
    state: &AppState,
    iface: &str,
    peers: &[WireguardPeer],
) -> Result<(), Box<Response>> {
    let path = peers_settings_path(iface);
    if let Err(message) = validate_peers(iface, peers) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(&path),
        )));
    }
    // Infallible: a peer is a struct of strings and integers.
    let value = encode(peers)?;
    if let Err(err) = state.api.set_settings(&path, &value).await {
        return Err(Box::new(bus_api_error(&err, Some(&path))));
    }
    Ok(())
}

/// List every peer configured on one WireGuard tunnel.
///
/// Answers **404** when `iface` names no declared interface.
#[utoipa::path(
    get,
    path = V1_NETWORK_PEERS_ROUTE,
    context_path = API,
    tag = "resources",
    params(("iface" = String, Path, description = "A declared `network` entry of kind `wireguard`")),
    responses(
        (status = 200, description = "The stored peers, in stored order, each with the public key that is its `DELETE` path segment", body = Vec<WireguardPeerEntry>),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "No `network` entry has that name (`settings_not_found`)", body = ApiError),
        (status = 422, description = "The name is not an interface name, or the entry is not a WireGuard one (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_peers_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(iface): Path<String>,
) -> Response {
    let path = peers_settings_path(&iface);
    match tunnel_peers(&state, &iface, &path).await {
        Ok(peers) => {
            // Through the shared redactor, as the WiFi listing is. No field of
            // a peer is on `redact`'s list today, so this substitutes nothing;
            // it is the fail-closed half of that rule, so the day a peer
            // pre-shared key enters the schema it is already covered.
            // Infallible: a peer is a struct of strings and integers.
            let value = match encode(&peers) {
                Ok(value) => value,
                Err(response) => return *response,
            };
            api_response(StatusCode::OK, redact::redact(value, &path))
        }
        Err(response) => *response,
    }
}

// 409 and not 422 for a duplicate: the public key IS this collection's
// identity — it is the DELETE path segment — so a second entry under one key
// would leave no answer to which of the two a DELETE names.
/// Add one peer to a WireGuard tunnel.
///
/// Answers **404** when `iface` names no declared interface, checked before
/// anything is written. A duplicate public key is **409** with code
/// `peer_exists`; a malformed peer is **422**.
#[utoipa::path(
    post,
    path = V1_NETWORK_PEERS_ROUTE,
    context_path = API,
    tag = "resources",
    params(("iface" = String, Path, description = "A declared `network` entry of kind `wireguard`")),
    request_body = WireguardPeerEntry,
    responses(
        (status = 201, description = "The peer was added; the body carries it back", body = WireguardPeerEntry),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No `network` entry has that name (`settings_not_found`); nothing is written", body = ApiError),
        (status = 409, description = "A stored peer already carries that public key (`peer_exists`); the key is this collection's identity, so the entry is not replaced silently", body = ApiError),
        (status = 422, description = "The name is not an interface name, the entry is not a WireGuard one, or the peer is one the reconciler would refuse -- a public key that is not 32 bytes of base64, an allowed IP that is not a CIDR, an endpoint that is not `host:port` (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_peers_add(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(iface): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let path = peers_settings_path(&iface);
    let peer: WireguardPeer = match json_body(body, Some(&path)) {
        Ok(peer) => peer,
        Err(response) => return *response,
    };
    let mut peers = match tunnel_peers(&state, &iface, &path).await {
        Ok(peers) => peers,
        Err(response) => return *response,
    };
    if peers
        .iter()
        .any(|stored| stored.public_key == peer.public_key)
    {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "peer_exists",
                // The key is echoed, unlike a validation refusal's message. It
                // is a public key by definition and the caller just sent it;
                // what the reconciler's index-only rule protects against is a
                // *rejected* value, which may be a private key pasted into the
                // wrong field, and a duplicate matched a value already stored.
                format!(
                    "a peer of network.{iface} already carries the public key `{}`; remove it before adding another under that key",
                    peer.public_key
                ),
            )
            .at(&path),
        );
    }
    // Serialized before the move into the list, so the echo is the entry as it
    // was stored and not the body as it arrived.
    let echoed = match encode(&peer) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    peers.push(peer);
    if let Err(response) = api_write_peers(&state, &iface, &peers).await {
        return *response;
    }
    api_response(StatusCode::CREATED, redact::redact(echoed, &path))
}

// The HTML pane answers 422 where this answers 404, deliberately: a form's
// body is a re-rendered page no consumer reads a status from.
/// Remove one peer from a tunnel, identified by its public key.
///
/// A string that is not 32 bytes of base64 could never be a WireGuard public
/// key and is **422**. A well-formed key that no stored peer carries is
/// **404**.
#[utoipa::path(
    delete,
    path = V1_NETWORK_PEER_ROUTE,
    context_path = API,
    tag = "resources",
    params(
        ("iface" = String, Path, description = "A declared `network` entry of kind `wireguard`"),
        ("publicKey" = String, Path, description = "The peer's public key, as `GET /api/v1/network/{iface}/peers` returns it: 32 bytes in padded base64. Its alphabet contains `/` and `+`, so a key carrying either is percent-encoded"),
    ),
    responses(
        (status = 204, description = "The peer was removed; the reconciler has re-rendered the tunnel without it"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No `network` entry has that name, or no peer of it carries that key (`settings_not_found`)", body = ApiError),
        (status = 422, description = "The interface name is not one, the entry is not a WireGuard one, or the path segment is not a WireGuard public key at all (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_peers_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((iface, public_key)): Path<(String, String)>,
) -> Response {
    let path = peers_settings_path(&iface);
    // Before the read, unlike the interface's own absence: a segment that
    // could never be a public key would send the caller looking for a peer
    // they deleted instead of at the URL they typed.
    if !is_wireguard_key(&public_key) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "a WireGuard public key is 32 bytes spelled in base64".to_string(),
            )
            .at(&path),
        );
    }
    let mut peers = match tunnel_peers(&state, &iface, &path).await {
        Ok(peers) => peers,
        Err(response) => return *response,
    };
    let Some(index) = peers.iter().position(|peer| peer.public_key == public_key) else {
        return item_not_found(&path, &public_key);
    };
    peers.remove(index);
    if let Err(response) = api_write_peers(&state, &iface, &peers).await {
        return *response;
    }
    no_content()
}
