//! The Wi-Fi client, its known networks and scanning.

use crate::assets::mime::CacheClass;
use crate::redact;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use micad_settings::WifiNetwork;
use serde_json::Value;

use super::*;

/// One known WiFi network, in both directions.
///
// Held field-for-field against `micad_settings::WifiNetwork` by a test, so a
// field added to the model cannot go undocumented here.
///
/// `psk` differs by direction. On the way **in** it is the pre-shared key. On
/// the way **out** it is `"<redacted>"` whenever a key is stored. A body
/// carrying the sentinel back is refused at **422** rather than written.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct WifiNetworkEntry {
    /// The network name, which is also this entry's `DELETE` path segment.
    pub(super) ssid: String,
    /// The pre-shared key; absent for an open network, `"<redacted>"` on
    /// every read of a network that has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) psk: Option<String>,
    /// Whether the network hides its SSID.
    pub(super) hidden: bool,
    /// Selection preference; higher wins.
    pub(super) priority: i32,
}

/// The stored WiFi networks, or the error envelope for whatever prevented reading
/// them.
///
/// The same shape as [`api_stored_keys`] and [`stored_tokens`], and an
/// unreadable list is an error for the same reason: reading it as empty would
/// let one `POST` drop every network the operator cannot see, including the one
/// the device is associated through.
pub(super) async fn stored_networks(state: &AppState) -> Result<Vec<WifiNetwork>, Box<Response>> {
    let value = match state.api.get_settings(WIFI_NETWORKS_PATH).await {
        Ok(value) => value,
        Err(err) => return Err(Box::new(bus_api_error(&err, Some(WIFI_NETWORKS_PATH)))),
    };
    // No absent-is-empty branch, unlike the two `access` lists above, and the
    // difference is in the model rather than in the route: `networks` carries
    // no `skip_serializing_if`, so micad's serialization of the typed tree
    // always has it, and a dot-path that does not resolve is micad's own
    // `NotFound` -- a 404 through `bus_api_error`, not an empty list.
    serde_json::from_value(value).map_err(|err| {
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored network list could not be read: {err}"),
            )
            .at(WIFI_NETWORKS_PATH),
        ))
    })
}

/// Write a rewritten network list.
///
/// There is no apid-side validator to run first, and that is a measured
/// statement rather than an omission. micad's own write path is
/// `Settings::set`, which validates by deserializing the candidate tree
/// (`Settings::set` in `micad-settings/src/model.rs`) -- so the typed
/// `WifiNetwork` this route deserializes into IS the validator micad runs — plus
/// `micad_settings::validate_wifi_psk`, so the crate holding the model states its
/// own field's rule.
pub(super) async fn write_networks(
    state: &AppState,
    networks: &[WifiNetwork],
) -> Result<(), Box<Response>> {
    let value = encode(networks)?;
    if let Err(err) = state.api.set_settings(WIFI_NETWORKS_PATH, &value).await {
        return Err(Box::new(bus_api_error(&err, Some(WIFI_NETWORKS_PATH))));
    }
    Ok(())
}

/// List every known WiFi network.
///
/// Each entry's pre-shared key is redacted. Posting an entry back with the
/// redaction sentinel still in place is refused rather than stored.
#[utoipa::path(
    get,
    path = V1_WIFI_NETWORKS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The stored networks, in stored order, each `psk` replaced by `\"<redacted>\"`", body = Vec<WifiNetworkEntry>),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a network list (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_networks_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match stored_networks(&state).await {
        Ok(networks) => {
            // Through `redact::redact` and not by rebuilding each entry field
            // by field. A second copy of the rule about which field names are
            // secret is a second opinion, and the whole argument for
            // a structural redactor is that the one list must be the only
            // list. Infallible: `WifiNetwork` is a struct of scalars with no
            // map keys that could collide.
            let value = match encode(&networks) {
                Ok(value) => value,
                Err(response) => return *response,
            };
            api_response(StatusCode::OK, redact::redact(value, WIFI_NETWORKS_PATH))
        }
        Err(response) => *response,
    }
}

// This collection has no HTML pane, so there is no form-path behaviour to
// stay consistent with.
/// Add one known WiFi network.
///
/// The redaction sentinel is rejected before anything else: a client that read
/// this list and posted an entry back would otherwise store the literal
/// `"<redacted>"` as the `psk` and destroy a working key.
///
/// Answers **422** for a malformed entry or a redacted `psk`.
#[utoipa::path(
    post,
    path = V1_WIFI_NETWORKS_PATH,
    context_path = API,
    tag = "resources",
    request_body = WifiNetworkEntry,
    responses(
        (status = 201, description = "The network was stored; the body carries it back with its `psk` redacted", body = WifiNetworkEntry),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "A stored network already carries that SSID (`ssid_exists`); the SSID is this collection's identity, so the entry is not replaced silently", body = ApiError),
        (status = 422, description = "The body carries the redaction sentinel, is not a network the settings model holds, or carries a `psk` outside IEEE 802.11i's 8..63 characters that is not a 64-digit hex PMK either (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a network list (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_networks_add(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(value) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()).at(WIFI_NETWORKS_PATH),
            );
        }
    };
    // Before the shape check and not after it, for the reason the scalar write
    // route gives: this is a rule about the body, and the client it protects --
    // one that read an entry, changed a field and posted the whole thing back
    // -- is answered about the thing it actually got wrong.
    if redact::carries_sentinel(&value) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "the body carries `{}`, which is what a read substitutes for a secret and never a value to write: writing it back would destroy the pre-shared key it stands for. Send the key itself, or omit `psk` for an open network",
                    redact::REDACTED
                ),
            )
            .at(WIFI_NETWORKS_PATH),
        );
    }
    let network: WifiNetwork = match serde_json::from_value(value) {
        Ok(network) => network,
        Err(err) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", err.to_string()).at(WIFI_NETWORKS_PATH),
            );
        }
    };
    // The pre-shared key's own bounds, run here for the first time. They could
    // not be run before: they lived inside
    // `encode_psk`, a private function of the `micad` binary crate's station
    // reconciler, so a key outside IEEE 802.11i's range was accepted, stored,
    // and refused later by the renderer with the error visible only in live
    // state. They live in `micad-settings` beside the typed model and the
    // reconciler calls the same copy, so this is the same rule and not a second
    // one that could disagree with the first.
    if let Some(psk) = &network.psk
        && let Err(message) = micad_settings::validate_wifi_psk(psk)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_NETWORKS_PATH),
        );
    }
    let mut networks = match stored_networks(&state).await {
        Ok(networks) => networks,
        Err(response) => return *response,
    };
    // 409 and not 422: the body is well formed and nothing about it is wrong,
    // and what refuses it is the collection's current state -- the condition
    // 409 already means, and the one the token mint's
    // `token_limit_reached` answers. Appending a second entry under one SSID
    // would also destroy the identity the item route below depends on: there
    // would be no answer to which of the two a `DELETE` names.
    if networks.iter().any(|stored| stored.ssid == network.ssid) {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "ssid_exists",
                format!(
                    "a stored network is already named `{}`; remove it before adding another under that SSID",
                    network.ssid
                ),
            )
            .at(WIFI_NETWORKS_PATH),
        );
    }
    // Redacted before the move into the list, and through the same shared
    // redactor the listing uses: what a `POST` echoes back must not be a
    // secret the `GET` beside it would have substituted.
    let echoed = match encode(&network) {
        Ok(value) => redact::redact(value, WIFI_NETWORKS_PATH),
        Err(response) => return *response,
    };
    networks.push(network);
    if let Err(response) = write_networks(&state, &networks).await {
        return *response;
    }
    api_response(StatusCode::CREATED, echoed)
}

// No 422 here, and that is the rule applied rather than an exception: an SSID
// has no grammar, so no path segment is malformed. Inventing a length bound to
// manufacture a 422 would refuse a network a hand-edited settings file holds.
/// Forget one known WiFi network by its SSID.
///
/// Answers **404** when no stored network carries that SSID. There is no
/// The station role: the radio it runs on and whether it runs.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WifiClientRole {
    /// Whether the station role is started.
    pub(super) enabled: bool,
    /// The wireless interface it runs on.
    pub(super) interface: String,
}

/// Read which radio the WiFi station runs on, and whether it runs.
///
/// The known networks are not here: they are their own collection, because
/// they carry secrets this document would then have to redact.
#[utoipa::path(
    get,
    path = V1_WIFI_CLIENT_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The station role", body = WifiClientRole),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The stored subtree is not a station document this build can read (`settings_invalid`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_client_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let stored = match state.api.get_settings(WIFI_CLIENT_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(WIFI_CLIENT_PATH)),
    };
    // Read field by field rather than deserialized whole: the subtree carries
    // the network list too, and this document is deliberately not that.
    let enabled = stored.get("enabled").and_then(Value::as_bool);
    let interface = stored
        .get("interface")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(enabled), Some(interface)) = (enabled, interface) else {
        return api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                "the stored WiFi station settings carry no `enabled` and `interface` pair"
                    .to_string(),
            )
            .at(WIFI_CLIENT_PATH),
        );
    };
    api_response(StatusCode::OK, WifiClientRole { enabled, interface })
}

/// Bind the WiFi station to a radio, and switch it on or off.
///
/// **The known networks are untouched**: they are written through their own
/// collection, and a route that replaced them here would drop every stored
/// pre-shared key the moment someone moved the station to another radio.
#[utoipa::path(
    put,
    path = V1_WIFI_CLIENT_PATH,
    context_path = API,
    tag = "resources",
    request_body = WifiClientRole,
    responses(
        (status = 202, description = "The station role was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape, or the interface is not an interface name (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_client_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let role: WifiClientRole = match json_body(body, Some(WIFI_CLIENT_PATH)) {
        Ok(role) => role,
        Err(response) => return *response,
    };
    if let Err(response) = check_iface_name(&role.interface, WIFI_CLIENT_PATH) {
        return *response;
    }
    // Two scalar writes and not one subtree write, deliberately: the subtree
    // holds the network list, and a whole-subtree write built from this body
    // would have to carry it -- which means reading it, redacting nothing, and
    // writing every pre-shared key back through the API on every switch flip.
    match state
        .api
        .set_settings(
            "wifi.client.interface",
            &Value::String(role.interface.clone()),
        )
        .await
    {
        Ok(_) => {}
        Err(err) => return bus_api_error(&err, Some("wifi.client.interface")),
    }
    match state
        .api
        .set_settings("wifi.client.enabled", &Value::Bool(role.enabled))
        .await
    {
        // The second task is the one returned: it is the write that starts or
        // stops the station, so it is the one whose outcome the caller is
        // waiting on.
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some("wifi.client.enabled")),
    }
}

/// Scan for WiFi networks on the station's radio.
///
/// **POST**, because it puts the radio to work: a scan sweeps every channel
/// and briefly costs the station its link. Nothing that merely follows a link
/// should do that.
///
/// The answer is micad's: `available` with the networks it found, or
/// `available: false` with the reason -- an interface with no station running
/// on it has no answer, and an empty list would claim there is nothing on the
/// air.
#[utoipa::path(
    post,
    path = V1_WIFI_SCAN_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 200, description = "What the radio found, or why it could not look", body = Object),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_scan(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.scan_wifi().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(WIFI_CLIENT_PATH)),
    }
}

/// Replace one known network, keeping its stored key unless a new one is sent.
///
/// **`psk` absent keeps the stored key.** An operator changing a network's
/// priority has not been shown the key -- a read substitutes the redaction
/// sentinel for it -- so a `PUT` that treated absence as "open network" would
/// silently drop the credential of a network that still works.
#[utoipa::path(
    put,
    path = V1_WIFI_NETWORK_ROUTE,
    context_path = API,
    tag = "resources",
    params(("ssid" = String, Path, description = "The stored network to replace")),
    request_body = WifiNetworkEntry,
    responses(
        (status = 200, description = "The stored entry, with its key redacted", body = WifiNetworkEntry),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No stored network carries this SSID (`not_found`)", body = ApiError),
        (status = 422, description = "The body is not a network, carries the redaction sentinel as its `psk`, names another SSID, or the key is outside IEEE 802.11i's length bounds (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored list could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_networks_replace(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(ssid): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(value) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()).at(WIFI_NETWORKS_PATH),
            );
        }
    };
    // The sentinel first, as the add route takes it: a client that read the
    // entry and posted it back is answered about the thing it got wrong.
    if redact::carries_sentinel(&value) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "the body carries `{}`, which is what a read substitutes for a secret and never a value to write. Omit `psk` to keep the stored key, or send the key itself",
                    redact::REDACTED
                ),
            )
            .at(WIFI_NETWORKS_PATH),
        );
    }
    let network: WifiNetwork = match serde_json::from_value(value) {
        Ok(network) => network,
        Err(err) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", err.to_string()).at(WIFI_NETWORKS_PATH),
            );
        }
    };
    // The path segment identifies the entry; a body that renames it is refused
    // rather than guessed at, because renaming a network and replacing it are
    // different operations and only one of them was asked for.
    if network.ssid != ssid {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "this route replaces `{ssid}`; the body names `{}`. Remove the entry and add the new one to rename it",
                    network.ssid
                ),
            )
            .at(WIFI_NETWORKS_PATH),
        );
    }
    if let Some(psk) = &network.psk
        && let Err(message) = micad_settings::validate_wifi_psk(psk)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_NETWORKS_PATH),
        );
    }
    let mut networks = match stored_networks(&state).await {
        Ok(networks) => networks,
        Err(response) => return *response,
    };
    let Some(index) = networks.iter().position(|stored| stored.ssid == ssid) else {
        return item_not_found(WIFI_NETWORKS_PATH, &ssid);
    };
    let mut network = network;
    if network.psk.is_none() {
        network.psk = networks[index].psk.clone();
    }
    let echoed = match encode(&network) {
        Ok(value) => redact::redact(value, WIFI_NETWORKS_PATH),
        Err(response) => return *response,
    };
    networks[index] = network;
    if let Err(response) = write_networks(&state, &networks).await {
        return *response;
    }
    api_response(StatusCode::OK, echoed)
}

/// malformed-SSID case: any non-empty path segment is a possible name.
#[utoipa::path(
    delete,
    path = V1_WIFI_NETWORK_ROUTE,
    context_path = API,
    tag = "resources",
    params(("ssid" = String, Path, description = "The network name, as `GET /api/v1/wifi/client/networks` returns it")),
    responses(
        (status = 204, description = "The network was forgotten; the station reconciler has re-rendered its configuration without it"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No stored network carries that SSID (`settings_not_found`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a network list (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_networks_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(ssid): Path<String>,
) -> Response {
    let mut networks = match stored_networks(&state).await {
        Ok(networks) => networks,
        Err(response) => return *response,
    };
    let Some(index) = networks.iter().position(|stored| stored.ssid == ssid) else {
        return item_not_found(WIFI_NETWORKS_PATH, &ssid);
    };
    networks.remove(index);
    if let Err(response) = write_networks(&state, &networks).await {
        return *response;
    }
    (
        StatusCode::NO_CONTENT,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
    )
        .into_response()
}

// The network cluster: the
// interface map, one interface, and a tunnel's peer collection.
