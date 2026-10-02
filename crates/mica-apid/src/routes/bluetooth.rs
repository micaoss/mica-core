//! Bluetooth: the adapter, discovery and paired devices.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The adapter's own settings: whether it runs, whether it answers scans, the
/// name it advertises and the code a legacy peer is told.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BluetoothAdapterSettings {
    /// Whether `bluetooth.service` runs and the adapter is powered.
    pub(super) enabled: bool,
    /// Whether the adapter answers a scan.
    pub(super) discoverable: bool,
    /// The name it advertises; absent advertises the hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) alias: Option<String>,
    /// The pairing code offered to a peer that asks for one. Absent derives
    /// one from the device identity. **Not a secret**: it is displayed,
    /// because somebody has to type it on the other device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pin: Option<String>,
}

/// The device's answer to a pending pairing confirmation.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PairingAnswer {
    /// Whether the passkey matched what the peer is showing.
    pub(super) accept: bool,
}

/// Whether a scan should be running.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DiscoveryRequest {
    /// `true` starts a scan, `false` stops it.
    pub(super) on: bool,
}

/// Read the Bluetooth surface: the trust list, the adapter, the devices, the
/// pairing code and whatever is waiting to be confirmed.
#[utoipa::path(
    get,
    path = V1_BLUETOOTH_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`declared`, `adapter`, `devices`, `pin` and `pending`, each named apart", body = Object),
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

/// Configure the adapter.
///
/// **The trust list is not here.** Devices arrive by pairing and leave by
/// being removed; a route that replaced the whole list would let a client
/// declare a device it never paired with, which the adapter would then have
/// no keys for.
#[utoipa::path(
    put,
    path = V1_BLUETOOTH_PATH,
    context_path = API,
    tag = "resources",
    request_body = BluetoothAdapterSettings,
    responses(
        (status = 202, description = "The adapter settings were written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the alias is empty or longer than 64 characters, or the pairing code is not 4 to 16 digits (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
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
    let requested: BluetoothAdapterSettings = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
        Ok(requested) => requested,
        Err(response) => return *response,
    };
    if let Some(pin) = &requested.pin
        && let Err(message) = micad_settings::validate_pairing_pin(pin)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(BLUETOOTH_SETTINGS_PATH),
        );
    }
    if let Some(alias) = &requested.alias
        && (alias.is_empty() || alias.len() > 64)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "a Bluetooth alias is 1 to 64 characters".to_string(),
            )
            .at(BLUETOOTH_SETTINGS_PATH),
        );
    }
    // The trust list is read from the tree and written back untouched: this
    // route configures the adapter, and the devices are the pairing verbs'.
    let stored = match state.api.get_settings(BLUETOOTH_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    };
    let mut subtree = match stored {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    subtree.insert("enabled".to_string(), Value::Bool(requested.enabled));
    subtree.insert(
        "discoverable".to_string(),
        Value::Bool(requested.discoverable),
    );
    set_or_remove(&mut subtree, "alias", requested.alias);
    set_or_remove(&mut subtree, "pin", requested.pin);
    match state
        .api
        .set_settings(BLUETOOTH_SETTINGS_PATH, &Value::Object(subtree))
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Write an optional string, or take the key out when it is absent.
///
/// Absent means "derive it", which is a different thing from an empty string,
/// so the key goes rather than being written as one.
pub(super) fn set_or_remove(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<String>,
) {
    match value {
        Some(value) => {
            map.insert(key.to_string(), Value::String(value));
        }
        None => {
            map.remove(key);
        }
    }
}

/// Start or stop a Bluetooth scan.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_DISCOVERY_PATH,
    context_path = API,
    tag = "actions",
    request_body = DiscoveryRequest,
    responses(
        (status = 204, description = "The adapter was told to start or stop scanning"),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape (`validation_failed`)", body = ApiError),
        (status = 500, description = "Bluetooth is switched off, this device has no adapter, or BlueZ refused (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_bluetooth_discovery(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: DiscoveryRequest = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    match state.api.set_bluetooth_discovery(request.on).await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Pair with a device, or answer the confirmation a pairing is waiting on.
///
/// Two verbs on one path because they are two halves of one exchange: `pair`
/// starts it, and `confirm` answers the passkey BlueZ asked about in the
/// middle of it.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_DEVICE_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("address" = String, Path, description = "The device's address, `AA:BB:CC:DD:EE:FF`"),
        ("action" = String, Path, description = "`pair` or `confirm`"),
    ),
    request_body(content = PairingAnswer, description = "`confirm` only: whether the passkey matched"),
    responses(
        (status = 204, description = "The pairing was started, or the confirmation was answered"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "The address is not a Bluetooth address, or no confirmation is waiting for it (`settings_rejected`)", body = ApiError),
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
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let result = match action.as_str() {
        "pair" => state.api.pair_bluetooth_device(&address).await,
        "confirm" => {
            let answer: PairingAnswer = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
                Ok(answer) => answer,
                Err(response) => return *response,
            };
            state
                .api
                .confirm_bluetooth_pairing(&address, answer.accept)
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
