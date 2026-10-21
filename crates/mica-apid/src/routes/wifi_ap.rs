//! The Wi-Fi access point, and the radio's role.

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
    /// `wpa2-psk`, `wpa2-wpa3` (WPA3 beside WPA2, which needs a passphrase
    /// rather than a raw PMK) or `open`; absent is `wpa2-psk`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) security: Option<String>,
    /// The DHCP pool, as an offset into the subnet of `address` and a count;
    /// absent is every address after the access point's own. It holds at
    /// least one address and neither the access point's own, the subnet
    /// address nor the broadcast address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pool: Option<AccessPointPool>,
    /// The DHCP lease time in seconds, at least 60; absent leaves the DHCP
    /// server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_seconds: Option<u32>,
    /// Write-only: `true` confirms a `countryCode` other than the stored one;
    /// without it such a write is 422 `wifi_country_unconfirmed`.
    #[serde(default, skip_serializing)]
    pub(super) country_confirmed: Option<bool>,
}

/// The access point's DHCP pool.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessPointPool {
    /// The first address handed out, as an offset from the subnet address.
    pub(super) offset: u32,
    /// How many addresses the pool holds.
    pub(super) size: u32,
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
///
/// A `countryCode` other than the stored one needs `countryConfirmed: true`.
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
        (status = 422, description = "The body is not this shape, the mode or the security is not one of the three, the interface is not an interface name, the key is outside IEEE 802.11i's bounds, the address is not IPv4 CIDR notation, the pool or the lease is one the DHCP server cannot serve, or the body carries the redaction sentinel (`validation_failed`); a new country without `countryConfirmed: true` (`wifi_country_unconfirmed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
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
    if let Err(response) = refuse_sentinel(&value, WIFI_AP_PATH) {
        return *response;
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
    let value = match stored_access_point(&state, requested).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(WIFI_AP_PATH, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(WIFI_AP_PATH)),
    }
}

/// Refuse a body that carries the redaction sentinel anywhere.
fn refuse_sentinel(value: &Value, path: &str) -> Result<(), Box<Response>> {
    if !redact::carries_sentinel(value) {
        return Ok(());
    }
    Err(Box::new(api_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::apid(
            "validation_failed",
            format!(
                "the body carries `{}`, which is what a read substitutes for a secret and never a value to write. Omit `psk` to keep the stored key",
                redact::REDACTED
            ),
        )
        .at(path),
    )))
}

/// The access point as a write stores it: checked, a new country confirmed,
/// and the stored key kept when the request sends none.
async fn stored_access_point(
    state: &AppState,
    requested: WifiAccessPoint,
) -> Result<Value, Box<Response>> {
    if !matches!(requested.mode.as_str(), "off" | "provisioning" | "always") {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the access point runs `off`, `provisioning` (only while no uplink works) or `always`".to_string(),
            )
            .at(WIFI_AP_PATH),
        )));
    }
    check_iface_name(&requested.interface, WIFI_AP_PATH)?;
    if let Some(psk) = &requested.psk
        && let Err(message) = micad_settings::validate_wifi_psk(psk)
    {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_AP_PATH),
        )));
    }
    if !valid_cidr(&requested.address) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the access-point address is IPv4 CIDR notation, e.g. 192.168.4.1/24".to_string(),
            )
            .at(WIFI_AP_PATH),
        )));
    }
    // The model's own rules for what this struct carries as plain values: the
    // security name and the pool against the address.
    let value = encode(&requested)?;
    if let Err(message) = serde_json::from_value::<micad_settings::WifiApSettings>(value)
        .map_err(|err| err.to_string())
        .and_then(|ap| micad_settings::validate_ap_pool(&ap))
    {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_AP_PATH),
        )));
    }
    let stored = state
        .api
        .get_settings(WIFI_AP_PATH)
        .await
        .map_err(|err| Box::new(bus_api_error(&err, Some(WIFI_AP_PATH))))?;
    let stored_country = stored.get("countryCode").and_then(Value::as_str);
    if stored_country.is_none_or(|stored| !stored.eq_ignore_ascii_case(&requested.country_code))
        && requested.country_confirmed != Some(true)
    {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "wifi_country_unconfirmed",
                format!(
                    "the radio's country changes to {}; confirm it with `countryConfirmed: true`",
                    requested.country_code
                ),
            )
            .with_params(serde_json::json!({ "countryCode": requested.country_code }))
            .at(WIFI_AP_PATH),
        )));
    }
    // The stored key is kept when the body sends none. Read from the tree and
    // not from the caller, so a client that never saw the key cannot drop it.
    let mut requested = requested;
    if requested.psk.is_none() {
        requested.psk = stored
            .get("psk")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    // Infallible: a struct of scalars.
    encode(&requested)
}

/// The radio's role.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WifiRole {
    /// Whether either role holds the radio.
    pub(super) enabled: bool,
    /// `client` or `ap`; `client` while off.
    pub(super) mode: &'static str,
    /// Whether another feature holds the Wi-Fi configuration; never, yet.
    pub(super) locked: bool,
}

/// One write of the radio's role and the configuration of that role.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WifiRoleWrite {
    /// `false` stops both roles.
    pub(super) enabled: bool,
    /// `client` or `ap`.
    pub(super) mode: String,
    /// The access point, required with `mode: ap` and refused otherwise.
    #[serde(default)]
    pub(super) ap: Option<WifiApConfig>,
    /// The station's configuration, only with `mode: client`.
    #[serde(default)]
    pub(super) client: Option<WifiClientConfig>,
}

/// The access point as the role write names it: `start` in place of `mode`,
/// which `enabled: false` turns off.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WifiApConfig {
    /// `always`, or `provisioning`: only while no uplink works.
    pub(super) start: String,
    pub(super) interface: String,
    #[serde(default)]
    pub(super) ssid: Option<String>,
    /// Absent keeps the stored key.
    #[serde(default)]
    pub(super) psk: Option<String>,
    pub(super) channel: u8,
    pub(super) country_code: String,
    /// `true` confirms a `countryCode` other than the stored one.
    #[serde(default)]
    pub(super) country_confirmed: Option<bool>,
    pub(super) address: String,
    pub(super) hold_down_seconds: u32,
    pub(super) grace_seconds: u32,
    #[serde(default)]
    pub(super) security: Option<String>,
    #[serde(default)]
    pub(super) pool: Option<AccessPointPool>,
    #[serde(default)]
    pub(super) lease_seconds: Option<u32>,
}

/// The station's part of the role write.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WifiClientConfig {
    /// The radio's own interface entry, replacing `network.<radio>`.
    #[schema(value_type = NetworkInterface)]
    pub(super) addressing: micad_settings::IfaceSettings,
}

/// Read which role holds the radio.
#[utoipa::path(
    get,
    path = V1_WIFI_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The radio's role: the access point's when it is on, else the station's", body = WifiRole),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let wifi = match state.api.get_settings(WIFI_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(WIFI_SETTINGS_PATH)),
    };
    let ap_on = wifi["ap"]["mode"]
        .as_str()
        .is_some_and(|mode| mode != "off");
    let client_on = wifi["client"]["enabled"].as_bool() == Some(true);
    api_response(
        StatusCode::OK,
        WifiRole {
            enabled: ap_on || client_on,
            mode: if ap_on { "ap" } else { "client" },
            locked: false,
        },
    )
}

/// Switch the radio's role and write that role's configuration, as one write
/// and one task.
///
/// `enabled: false` stops both roles. `mode: client` starts the station and
/// stops the access point, and `client.addressing` replaces the radio's
/// interface entry. `mode: ap` stops the station and writes `ap` with `start`
/// as its mode; `psk` absent keeps the stored key.
#[utoipa::path(
    put,
    path = V1_WIFI_PATH,
    context_path = API,
    tag = "resources",
    request_body = WifiRoleWrite,
    responses(
        (status = 202, description = "The role and its configuration were written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The write carries the radio's addressing while a network task has not finished (`busy`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the mode is not `client` or `ap`, a role's configuration comes without its mode, the access point or the addressing is one its own route refuses, or the body carries the redaction sentinel (`validation_failed`); a new country without `countryConfirmed: true` (`wifi_country_unconfirmed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Ok(Json(value)) = &body
        && let Err(response) = refuse_sentinel(value, WIFI_SETTINGS_PATH)
    {
        return *response;
    }
    let request: WifiRoleWrite = match json_body(body, Some(WIFI_SETTINGS_PATH)) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let refusal = match (request.enabled, request.mode.as_str()) {
        (_, mode) if mode != "client" && mode != "ap" => Some("the radio runs as `client` or `ap`"),
        (true, "ap") if request.ap.is_none() => Some("`mode: ap` names the access point in `ap`"),
        (true, "ap") if request.client.is_some() => Some("`client` goes with `mode: client`"),
        (true, "client") if request.ap.is_some() => Some("`ap` goes with `mode: ap`"),
        (false, _) if request.ap.is_some() || request.client.is_some() => {
            Some("a radio switched off takes no role configuration")
        }
        _ => None,
    };
    if let Some(message) = refusal {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message.to_string()).at(WIFI_SETTINGS_PATH),
        );
    }
    let mut writes = vec![(
        "wifi.client.enabled".to_string(),
        Value::Bool(request.enabled && request.mode == "client"),
    )];
    if let Some(ap) = request.ap {
        if !matches!(ap.start.as_str(), "always" | "provisioning") {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid(
                    "validation_failed",
                    "the access point starts `always` or `provisioning` (only while no uplink works)"
                        .to_string(),
                )
                .at(WIFI_AP_PATH),
            );
        }
        let requested = WifiAccessPoint {
            mode: ap.start,
            interface: ap.interface,
            ssid: ap.ssid,
            psk: ap.psk,
            channel: ap.channel,
            country_code: ap.country_code,
            address: ap.address,
            hold_down_seconds: ap.hold_down_seconds,
            grace_seconds: ap.grace_seconds,
            security: ap.security,
            pool: ap.pool,
            lease_seconds: ap.lease_seconds,
            country_confirmed: ap.country_confirmed,
        };
        match stored_access_point(&state, requested).await {
            Ok(value) => writes.push((WIFI_AP_PATH.to_string(), value)),
            Err(response) => return *response,
        }
    } else {
        writes.push(("wifi.ap.mode".to_string(), Value::String("off".to_string())));
    }
    if let Some(client) = request.client {
        match radio_addressing(&state, client.addressing).await {
            Ok(write) => writes.push(write),
            Err(response) => return *response,
        }
    }
    match state.api.set_settings_many(&writes).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(WIFI_SETTINGS_PATH)),
    }
}

/// The station radio's interface entry as a write stores it, checked as the
/// per-interface route checks one.
async fn radio_addressing(
    state: &AppState,
    cfg: micad_settings::IfaceSettings,
) -> Result<(String, Value), Box<Response>> {
    let radio = state
        .api
        .get_settings("wifi.client.interface")
        .await
        .map_err(|err| Box::new(bus_api_error(&err, Some("wifi.client.interface"))))?;
    let Some(radio) = radio.as_str().map(str::to_string) else {
        return Err(Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                "the stored WiFi station settings name no interface".to_string(),
            )
            .at(WIFI_CLIENT_PATH),
        )));
    };
    let path = iface_settings_path(&radio);
    check_network_idle(state)?;
    let mut candidate = api_network_entries(state).await?;
    candidate.insert(radio.clone(), cfg.clone());
    relational_refusal(&candidate, &path)?;
    address_refusal(&radio, &cfg)?;
    routing_refusal(&radio, &cfg)?;
    Ok((path, encode(&cfg)?))
}
