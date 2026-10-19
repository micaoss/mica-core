//! The Wi-Fi access point.

use crate::redact;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The access point, as the device holds it.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WifiAccessPoint {
    /// `off`, `provisioning` (only while no uplink works) or `always`.
    pub(super) mode: String,
    /// The wireless interface it runs on.
    pub(super) interface: String,
    /// The advertised name; absent derives one from the device identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ssid: Option<String>,
    /// The pre-shared key. **Read as `<redacted>`**; absent on a write keeps
    /// the stored one, and absent on a device that never had one derives it
    /// from the device credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) psk: Option<String>,
    /// The 2.4 GHz channel.
    pub(super) channel: u8,
    /// The regulatory domain the radio is configured for.
    pub(super) country_code: String,
    /// The AP-side address in CIDR notation.
    pub(super) address: String,
    /// Seconds without a usable uplink before the access point starts.
    pub(super) hold_down_seconds: u32,
    /// Seconds it stays up after an uplink returns.
    pub(super) grace_seconds: u32,
}

/// Read the access-point configuration, with its key redacted.
#[utoipa::path(
    get,
    path = V1_WIFI_AP_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The `wifi.ap` subtree; `psk` reads as `<redacted>`", body = WifiAccessPoint),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The stored subtree is not an access-point document this build can read (`settings_invalid`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_ap_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let stored = match state.api.get_settings(WIFI_AP_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(WIFI_AP_PATH)),
    };
    // Through the same redactor every other read uses: the key is `psk`, the
    // one name `SECRET_FIELDS` carries for exactly this pair of fields.
    api_response(StatusCode::OK, redact::redact(stored, WIFI_AP_PATH))
}

/// Configure the access point: switch it on, name it, key it, place it.
///
/// **`psk` absent keeps the stored key**, for the reason the known-network
/// route gives: a read substitutes the redaction sentinel, so an operator
/// changing the channel has not been shown the key, and treating absence as
/// "no key" would publish an open access point.
#[utoipa::path(
    put,
    path = V1_WIFI_AP_PATH,
    context_path = API,
    tag = "resources",
    request_body = WifiAccessPoint,
    responses(
        (status = 202, description = "The configuration was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the mode is not one of the three, the interface is not an interface name, the key is outside IEEE 802.11i's bounds, the address is not IPv4 CIDR notation, or the body carries the redaction sentinel (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The stored subtree could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_ap_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(value) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()).at(WIFI_AP_PATH),
            );
        }
    };
    if redact::carries_sentinel(&value) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "the body carries `{}`, which is what a read substitutes for a secret and never a value to write. Omit `psk` to keep the stored key",
                    redact::REDACTED
                ),
            )
            .at(WIFI_AP_PATH),
        );
    }
    let requested: WifiAccessPoint = match serde_json::from_value(value) {
        Ok(requested) => requested,
        Err(err) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", err.to_string()).at(WIFI_AP_PATH),
            );
        }
    };
    if !matches!(requested.mode.as_str(), "off" | "provisioning" | "always") {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the access point runs `off`, `provisioning` (only while no uplink works) or `always`".to_string(),
            )
            .at(WIFI_AP_PATH),
        );
    }
    if let Err(response) = check_iface_name(&requested.interface, WIFI_AP_PATH) {
        return *response;
    }
    if let Some(psk) = &requested.psk
        && let Err(message) = micad_settings::validate_wifi_psk(psk)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_AP_PATH),
        );
    }
    if !valid_cidr(&requested.address) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the access-point address is IPv4 CIDR notation, e.g. 192.168.4.1/24".to_string(),
            )
            .at(WIFI_AP_PATH),
        );
    }
    // The stored key is kept when the body sends none. Read from the tree and
    // not from the caller, so a client that never saw the key cannot drop it.
    let stored = match state.api.get_settings(WIFI_AP_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(WIFI_AP_PATH)),
    };
    let mut requested = requested;
    if requested.psk.is_none() {
        requested.psk = stored
            .get("psk")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    // Infallible: a struct of scalars.
    let value = match encode(&requested) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(WIFI_AP_PATH, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(WIFI_AP_PATH)),
    }
}
