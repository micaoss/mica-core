//! Bluetooth: the adapter, discovery and paired devices.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The adapter's one setting: whether it runs.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BluetoothAdapterSettings {
    /// Whether `bluetooth.service` runs and the adapter is powered.
    pub(super) powered: bool,
}

/// What the adapter no longer takes: it is discoverable only while a pairing
/// window is open, advertises the hostname, and offers no fixed code.
const RETIRED_FIELDS: &[&str] = &["discoverable", "alias", "pairingCode", "pin"];

/// The device's answer to a pending pairing confirmation.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PairingAnswer {
    /// Whether the six digits match what the peer is showing.
    pub(super) accept: bool,
}

/// When the pairing window closes by itself.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PairWindow {
    /// Seconds since the epoch.
    pub(super) expires_at: u64,
}

/// Read the Bluetooth surface: the trust list, the adapter, the devices, the
/// pairing window and whatever is waiting to be confirmed.
#[utoipa::path(
    get,
    path = V1_BLUETOOTH_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`enabled`, `declared`, `adapter` (`name`, `address`, `powered`, `discoverable`), `devices`, `pending` (`name`, `mac`, `passkey`) and `pairWindow` (`expiresAt`, or null), each named apart", body = Object),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_bluetooth().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Switch the adapter on or off.
///
/// **The trust list is not here.** Devices arrive by pairing and leave by
/// being removed; a route that replaced the whole list would let a client
/// declare a device it never paired with, which the adapter would then have
/// no keys for. Nor is discoverability, the name or a pairing code: the
/// adapter is discoverable only while a pairing window is open, advertises
/// the hostname, and offers no fixed code.
#[utoipa::path(
    put,
    path = V1_BLUETOOTH_PATH,
    context_path = API,
    tag = "resources",
    request_body = BluetoothAdapterSettings,
    responses(
        (status = 202, description = "The switch was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body carries `discoverable`, `alias`, `pairingCode` or `pin` (`not_supported`, params `field`), or is not this shape (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The stored subtree could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Ok(Json(Value::Object(map))) = &body
        && let Some(field) = RETIRED_FIELDS
            .iter()
            .find(|field| map.contains_key(**field))
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "not_supported",
                format!(
                    "`{field}` is not set on this device: it is discoverable only while a \
                     pairing window is open, advertises its hostname, and offers no fixed code"
                ),
            )
            .with_params(serde_json::json!({ "field": field }))
            .at(BLUETOOTH_SETTINGS_PATH),
        );
    }
    let requested: BluetoothAdapterSettings = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
        Ok(requested) => requested,
        Err(response) => return *response,
    };
    match state
        .api
        .set_settings(
            &format!("{BLUETOOTH_SETTINGS_PATH}.enabled"),
            &Value::Bool(requested.powered),
        )
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Open the pairing window: for 180 seconds the adapter is discoverable and
/// pairable, and a pairing waits in `pending` for the console. Opening it
/// again restarts the 180 seconds.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_PAIR_WINDOW_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 200, description = "The window is open until `expiresAt`", body = PairWindow),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 500, description = "Bluetooth is switched off, this device has no adapter, or BlueZ refused (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_pair_window_open(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.open_bluetooth_pair_window().await {
        Ok(expires_at) => api_response(StatusCode::OK, PairWindow { expires_at }),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Close the pairing window now; whatever still waits is refused.
#[utoipa::path(
    delete,
    path = V1_BLUETOOTH_PAIR_WINDOW_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 204, description = "The window is closed"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 500, description = "BlueZ refused (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_pair_window_close(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.close_bluetooth_pair_window().await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Answer a pairing waiting in `pending`: `accept` when the six digits match
/// what the other device shows. An accepted device joins the trust list.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_PAIRING_REQUEST_ROUTE,
    context_path = API,
    tag = "actions",
    params(("mac" = String, Path, description = "The waiting device's address, `AA:BB:CC:DD:EE:FF`")),
    request_body = PairingAnswer,
    responses(
        (status = 204, description = "The pairing was answered"),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape (`validation_failed`)", body = ApiError),
        (status = 500, description = "No pairing of this address is waiting (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_pairing_request(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(mac): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let answer: PairingAnswer = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
        Ok(answer) => answer,
        Err(response) => return *response,
    };
    match state
        .api
        .confirm_bluetooth_pairing(&mac, answer.accept)
        .await
    {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Act on a device: `pair` with one this device started the pairing with;
/// `trust`, `untrust`, `block` or `unblock` a declared one. A paired device
/// starts untrusted.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_DEVICE_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("address" = String, Path, description = "The device's address, `AA:BB:CC:DD:EE:FF`"),
        ("action" = String, Path, description = "`pair`, `trust`, `untrust`, `block` or `unblock`"),
    ),
    responses(
        (status = 204, description = "The pairing was started, or the flag set"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "The address is not a Bluetooth address, or no device of it is declared (`settings_rejected`)", body = ApiError),
        (status = 500, description = "Bluetooth is switched off, this device has no adapter, or the pairing failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_device_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((address, action)): Path<(String, String)>,
) -> Response {
    let result = match action.as_str() {
        "pair" => state.api.pair_bluetooth_device(&address).await,
        "trust" | "untrust" => {
            state
                .api
                .set_bluetooth_device_flag(&address, "trusted", action == "trust")
                .await
        }
        "block" | "unblock" => {
            state
                .api
                .set_bluetooth_device_flag(&address, "blocked", action == "block")
                .await
        }
        // An allowlist and not a passthrough: a path segment that is not one
        // of these names nothing.
        _ => return item_not_found(BLUETOOTH_SETTINGS_PATH, &action),
    };
    match result {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Drop a device: out of the trust list, and out of the adapter's keys.
#[utoipa::path(
    delete,
    path = V1_BLUETOOTH_DEVICE_ROUTE,
    context_path = API,
    tag = "resources",
    params(("address" = String, Path, description = "The declared device's address")),
    responses(
        (status = 204, description = "The device was removed"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "No device of this address is declared (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_device_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Response {
    match state.api.remove_bluetooth_device(&address).await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}
