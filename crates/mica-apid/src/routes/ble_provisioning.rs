//! BLE provisioning (`docs/ble-provisioning.md`): its state, the console's
//! `manual` window and the offline trigger's switch.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// What opened the window.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProvisioningTrigger {
    Unclaimed,
    Boot,
    Offline,
    Manual,
}

/// One finished connection.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisioningEntry {
    /// Seconds since the epoch.
    pub(super) at: u64,
    pub(super) trigger: ProvisioningTrigger,
    /// `claimed`, `finished`, `wifi-connected`, `auth-failed` or `abandoned`.
    pub(super) outcome: String,
}

/// BLE provisioning's state.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisioningStatus {
    /// `off`, `waiting`, `connected` or `done`.
    pub(super) state: String,
    /// What opened the window; null while none is open.
    pub(super) trigger: Option<ProvisioningTrigger>,
    /// Seconds since the epoch the window closes at; null while none is
    /// open, or while `offline` waits for the default route to return.
    pub(super) expires_at: Option<u64>,
    /// The `manual` window's eight-digit temp code; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) code: Option<String>,
    /// Whether losing the default route for 120 seconds opens `offline`.
    pub(super) auto_when_offline: bool,
    /// Whether the administrator password works over Bluetooth; false until
    /// the next console login on a device claimed before this build.
    pub(super) verifier_ready: bool,
    /// Seconds since the epoch a lockout ends at; null when none holds.
    pub(super) locked_until: Option<u64>,
    /// Whether a session holds Wi-Fi: its writes answer 423 `wifi_locked`.
    pub(super) wifi_locked: bool,
    /// The last ten connections, newest first.
    pub(super) history: Vec<ProvisioningEntry>,
}

/// How long a `manual` window stays open.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProvisioningOpen {
    /// 300, 600 or 1800.
    pub(super) duration_seconds: u64,
}

/// The offline trigger's switch.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProvisioningSettings {
    /// Whether losing the default route for 120 seconds opens a window.
    pub(super) auto_when_offline: bool,
}

/// The lengths a `manual` window may have.
const DURATIONS: [u64; 3] = [300, 600, 1800];

/// The settings dot-path of the offline trigger's switch.
const AUTO_WHEN_OFFLINE_PATH: &str = "bluetooth.provisioning.autoWhenOffline";

/// Read BLE provisioning's state.
#[utoipa::path(
    get,
    path = V1_BLUETOOTH_PROVISIONING_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The state; `code` only while a `manual` window is open", body = ProvisioningStatus),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "BLE provisioning is not running on this device (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ble_provisioning_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_ble_provisioning().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Open a `manual` window: a phone authenticates with the temp code the
/// answer carries. Opening it again replaces the code and the length.
#[utoipa::path(
    post,
    path = V1_BLUETOOTH_PROVISIONING_PATH,
    context_path = API,
    tag = "actions",
    request_body = ProvisioningOpen,
    responses(
        (status = 200, description = "The window is open; the state carries `code`", body = ProvisioningStatus),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device has no administrator yet (`not_claimed`), or too many failed attempts locked provisioning (`locked`)", body = ApiError),
        (status = 422, description = "`durationSeconds` is not 300, 600 or 1800 (`invalid_duration`), or the body is not this shape (`validation_failed`)", body = ApiError),
        (status = 500, description = "BLE provisioning is not running on this device (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ble_provisioning_open(
    credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let open: ProvisioningOpen = match json_body(body, Some(BLUETOOTH_SETTINGS_PATH)) {
        Ok(open) => open,
        Err(response) => return *response,
    };
    if !DURATIONS.contains(&open.duration_seconds) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "invalid_duration",
                "a provisioning window lasts 300, 600 or 1800 seconds".to_string(),
            )
            .at(BLUETOOTH_SETTINGS_PATH),
        );
    }
    if let Err(err) = state.api.open_ble_provisioning(open.duration_seconds).await {
        return open_refused(&err)
            .unwrap_or_else(|| bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)));
    }
    api_v1_ble_provisioning_read(credential, State(state)).await
}

/// micad's `AccessDenied` refusal of a `manual` window, `reason: message`,
/// as its 409.
fn open_refused(err: &anyhow::Error) -> Option<Response> {
    let (name, message) = err.downcast_ref::<zbus::Error>().and_then(error_reply)?;
    if name != FDO_ACCESS_DENIED {
        return None;
    }
    let (reason, message) = message?
        .split_once(": ")
        .map(|(reason, message)| (reason.to_string(), message.to_string()))?;
    let code = ["not_claimed", "locked"]
        .into_iter()
        .find(|code| *code == reason)?;
    Some(api_response(
        StatusCode::CONFLICT,
        ApiError::micad(code, message).at(BLUETOOTH_SETTINGS_PATH),
    ))
}

/// Close the `manual` window; its temp code stops working. A window opened
/// by boot or by losing the network stays.
#[utoipa::path(
    delete,
    path = V1_BLUETOOTH_PROVISIONING_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 204, description = "The `manual` window is closed"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ble_provisioning_close(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.close_ble_provisioning().await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(BLUETOOTH_SETTINGS_PATH)),
    }
}

/// Switch the offline trigger: whether a claimed device that loses its
/// default route for 120 seconds opens a window.
#[utoipa::path(
    put,
    path = V1_BLUETOOTH_PROVISIONING_SETTINGS_PATH,
    context_path = API,
    tag = "resources",
    request_body = ProvisioningSettings,
    responses(
        (status = 202, description = "The switch was written; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape (`validation_failed`), or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ble_provisioning_settings(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let requested: ProvisioningSettings = match json_body(body, Some(AUTO_WHEN_OFFLINE_PATH)) {
        Ok(requested) => requested,
        Err(response) => return *response,
    };
    match state
        .api
        .set_settings(
            AUTO_WHEN_OFFLINE_PATH,
            &Value::Bool(requested.auto_when_offline),
        )
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(AUTO_WHEN_OFFLINE_PATH)),
    }
}
