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
    /// The DHCP server clients get their addresses from. A read carries it
    /// whenever `address` is valid; absent on a write is every address after
    /// the access point's own and the server's default lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dhcp_server: Option<AccessPointDhcp>,
    /// Write-only: `true` confirms a `countryCode` other than the stored one;
    /// without it such a write is 422 `wifi_country_unconfirmed`.
    #[serde(default, skip_serializing)]
    pub(super) country_confirmed: Option<bool>,
}

/// The access point's DHCP server.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AccessPointDhcp {
    /// The first address handed out, inside the subnet of `address`.
    pub(super) pool_start: String,
    /// The last address handed out. The pool holds neither the access
    /// point's own address, the subnet address nor the broadcast address.
    pub(super) pool_end: String,
    /// The lease time in seconds, at least 60; absent leaves the DHCP
    /// server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease_seconds: Option<u32>,
}

/// An IPv4 CIDR as its address, subnet address and broadcast address.
fn subnet_of(cidr: &str) -> Option<(u32, u32, u32)> {
    let (address, prefix) = cidr.split_once('/')?;
    let address = u32::from(address.parse::<std::net::Ipv4Addr>().ok()?);
    let prefix = prefix.parse::<u32>().ok().filter(|prefix| *prefix <= 32)?;
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    Some((address, address & mask, (address & mask) | !mask))
}

/// The pool micad serves when none is stored: every address after the access
/// point's own, up to the one before the broadcast address.
fn derived_pool(address: u32, broadcast: u32) -> Option<(u32, u32)> {
    (address.checked_add(1)? < broadcast).then(|| (address + 1, broadcast - 1))
}

/// The stored pool and lease beside `address`, as a read shows them.
fn dhcp_document(address: &str, pool: &Value, lease: Option<u32>) -> Option<AccessPointDhcp> {
    let (own, net, broadcast) = subnet_of(address)?;
    let offset = pool.get("offset").and_then(Value::as_u64);
    let size = pool.get("size").and_then(Value::as_u64);
    let (start, end) = match (offset, size) {
        (Some(offset), Some(size)) if size > 0 => {
            let start = u32::try_from(u64::from(net) + offset).ok()?;
            (start, u32::try_from(u64::from(start) + size - 1).ok()?)
        }
        _ => derived_pool(own, broadcast)?,
    };
    Some(AccessPointDhcp {
        pool_start: std::net::Ipv4Addr::from(start).to_string(),
        pool_end: std::net::Ipv4Addr::from(end).to_string(),
        lease_seconds: lease,
    })
}

/// The pool a write names, as the stored offset and size: `None` when it is
/// the derived pool, so it keeps following the address.
fn stored_pool(address: &str, dhcp: &AccessPointDhcp) -> Result<Option<(u32, u32)>, Box<Response>> {
    let refusal = |error: ApiError| {
        Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.at(WIFI_AP_PATH),
        ))
    };
    let Some((own, net, broadcast)) = subnet_of(address) else {
        return Ok(None);
    };
    let parse = |text: &str| {
        text.parse::<std::net::Ipv4Addr>()
            .map(u32::from)
            .map_err(|_| {
                refusal(ApiError::apid(
                    "validation_failed",
                    format!("{text:?} is not an IPv4 address"),
                ))
            })
    };
    let (start, end) = (parse(&dhcp.pool_start)?, parse(&dhcp.pool_end)?);
    let refused = |code: &'static str, message: String| {
        Err(refusal(ApiError::apid(code, message).with_params(
            serde_json::json!({ "poolStart": dhcp.pool_start, "poolEnd": dhcp.pool_end }),
        )))
    };
    if [start, end].iter().any(|ip| *ip <= net || *ip >= broadcast) {
        return refused(
            "pool_out_of_subnet",
            format!("the pool lies inside {address}, without its subnet and broadcast addresses"),
        );
    }
    if start > end {
        return refused("pool_order", "the pool ends before it starts".to_string());
    }
    if (start..=end).contains(&own) {
        return refused(
            "pool_contains_address",
            format!(
                "the pool cannot hold the access point's own address {}",
                std::net::Ipv4Addr::from(own)
            ),
        );
    }
    if derived_pool(own, broadcast) == Some((start, end)) {
        return Ok(None);
    }
    Ok(Some((start - net, end - start + 1)))
}

/// The stored access point as a read shows it: the pool and lease as
/// `dhcpServer`.
fn ap_document(mut stored: Value) -> Value {
    let Some(object) = stored.as_object_mut() else {
        return stored;
    };
    let pool = object.remove("pool").unwrap_or(Value::Null);
    let lease = object
        .remove("leaseSeconds")
        .and_then(|lease| lease.as_u64())
        .and_then(|lease| u32::try_from(lease).ok());
    let address = object
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(dhcp) = dhcp_document(address, &pool, lease)
        && let Ok(value) = serde_json::to_value(dhcp)
    {
        object.insert("dhcpServer".to_string(), value);
    }
    stored
}

/// The access point as `wifi.ap` stores it: the pool as an offset and a count
/// into the subnet, and the lease beside it.
fn stored_form(requested: &WifiAccessPoint) -> Result<Value, Box<Response>> {
    let mut value = encode(requested)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("dhcpServer");
        if let Some(dhcp) = &requested.dhcp_server {
            let pool = stored_pool(&requested.address, dhcp)?;
            if let Some((offset, size)) = pool {
                object.insert(
                    "pool".to_string(),
                    serde_json::json!({ "offset": offset, "size": size }),
                );
            }
            if let Some(lease) = dhcp.lease_seconds {
                object.insert("leaseSeconds".to_string(), lease.into());
            }
        }
    }
    Ok(value)
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
    api_response(
        StatusCode::OK,
        ap_document(redact::redact(stored, WIFI_AP_PATH)),
    )
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
        (status = 422, description = "The body is not this shape, the mode or the security is not one of the three, the interface is not an interface name, the key is outside IEEE 802.11i's bounds, the address is not IPv4 CIDR notation, the lease is one the DHCP server cannot serve, or the body carries the redaction sentinel (`validation_failed`); a pool outside the subnet (`pool_out_of_subnet`), ending before it starts (`pool_order`) or holding the access point's address (`pool_contains_address`); a new country without `countryConfirmed: true` (`wifi_country_unconfirmed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
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
    let value = stored_form(&requested)?;
    if let Err(message) = serde_json::from_value::<micad_settings::WifiApSettings>(value)
        .map_err(|err| err.to_string())
        .and_then(|ap| micad_settings::validate_ap_pool(&ap))
    {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(WIFI_AP_PATH),
        )));
    }
    if requested.channel != 0
        && !micad_settings::ap_channels(&requested.country_code).contains(&requested.channel)
    {
        let allowed = micad_settings::ap_channels(&requested.country_code);
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "channel_not_allowed",
                format!(
                    "channel {} is not allowed in {}: use {} to {}, or 0 for automatic",
                    requested.channel,
                    requested.country_code,
                    allowed.start(),
                    allowed.end()
                ),
            )
            .with_params(serde_json::json!({
                "channel": requested.channel,
                "countryCode": requested.country_code,
            }))
            .at(WIFI_AP_PATH),
        )));
    }
    // Refused only when the radio said what it offers: a radio not asked yet
    // is not one known to lack it.
    if requested.security.as_deref() == Some("wpa2-wpa3")
        && radio_capabilities(state)
            .await
            .and_then(|capabilities| capabilities.get("security").cloned())
            .and_then(|security| security.as_array().cloned())
            .is_some_and(|security| !security.iter().any(|offered| offered == "wpa2-wpa3"))
    {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "not_supported",
                "this radio cannot run WPA3 as an access point".to_string(),
            )
            .with_params(serde_json::json!({ "security": "wpa2-wpa3" }))
            .at(WIFI_AP_PATH),
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
    stored_form(&requested)
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
    pub(super) dhcp_server: Option<AccessPointDhcp>,
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
        (status = 422, description = "The body is not this shape, the mode is not `client` or `ap`, a role's configuration comes without its mode, the access point or the addressing is one its own route refuses, or the body carries the redaction sentinel (`validation_failed`); an access-point pool its own route refuses (`pool_out_of_subnet`, `pool_order`, `pool_contains_address`); a new country without `countryConfirmed: true` (`wifi_country_unconfirmed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
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
            dhcp_server: ap.dhcp_server,
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
    check_network_idle(state).await?;
    let mut candidate = api_network_entries(state).await?;
    candidate.insert(radio.clone(), cfg.clone());
    map_refusal(&candidate, &[radio.as_str()], &path)?;
    Ok((path, encode(&cfg)?))
}

/// `capabilities.wifi` of the observed network, once the device has said
/// what its radio offers.
async fn radio_capabilities(state: &AppState) -> Option<Value> {
    let observed = state.api.get_observed_network().await.ok()?;
    let wifi = observed.pointer("/capabilities/wifi")?;
    wifi.get("security").is_some().then(|| wifi.clone())
}

/// What the radio can do as an access point.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WifiCapabilities {
    /// Whether the device said; when not, the other members are what every
    /// radio this image drives offers.
    pub(super) available: bool,
    /// The security types an access point can offer: `wpa2-psk`,
    /// `wpa2-wpa3` where SAE and protected management frames run, `open`.
    pub(super) security: Vec<String>,
    /// The bands the radio has: `2.4`, `5`, `6`.
    pub(super) bands: Vec<String>,
    /// Whether the access point can choose its channel itself (channel 0).
    pub(super) acs: bool,
    /// Whether the radio can be a station and an access point at once.
    pub(super) ap_client_concurrent: bool,
}

/// Read what the radio can do as an access point.
///
/// From `iw`, asked once per daemon: the security types, the bands, whether
/// the access point can choose its channel (the driver keeps a survey and
/// hostapd was built with ACS) and whether a station and an access point can
/// run at once. A device that has not said answers `available: false` with
/// WPA2 and open on 2.4 GHz.
#[utoipa::path(
    get,
    path = V1_WIFI_CAPABILITIES_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "What the radio offers", body = WifiCapabilities),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_capabilities(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let strings = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let capabilities = match radio_capabilities(&state).await {
        Some(found) => WifiCapabilities {
            available: true,
            security: strings(found.get("security")),
            bands: strings(found.get("bands")),
            acs: found.get("acs").and_then(Value::as_bool).unwrap_or(false),
            ap_client_concurrent: found
                .get("apClientConcurrent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        None => WifiCapabilities {
            available: false,
            security: vec!["wpa2-psk".to_string(), "open".to_string()],
            bands: vec!["2.4".to_string()],
            acs: false,
            ap_client_concurrent: false,
        },
    };
    api_response(StatusCode::OK, capabilities)
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct WifiChannelsQuery {
    /// The two-letter regulatory domain.
    pub(super) country: String,
}

/// The channels an access point may use in one country.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WifiChannels {
    pub(super) country_code: String,
    /// The 2.4 GHz channels, in order.
    pub(super) channels: Vec<u8>,
    /// Whether channel 0, the access point's own choice, is offered.
    pub(super) auto: bool,
}

/// Read the channels an access point may use in a country.
///
/// The 2.4 GHz channels the country allows -- 1 to 11 under the FCC's plan,
/// 1 to 13 elsewhere -- that the radio has, and whether it can choose one
/// itself.
#[utoipa::path(
    get,
    path = V1_WIFI_CHANNELS_PATH,
    context_path = API,
    tag = "resources",
    params(WifiChannelsQuery),
    responses(
        (status = 200, description = "The allowed channels", body = WifiChannels),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 422, description = "`country` is missing or not a two-letter code (`validation_failed`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_wifi_channels(
    _credential: ApiCredential,
    State(state): State<AppState>,
    query: Result<
        axum::extract::Query<WifiChannelsQuery>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Response {
    let country = match query {
        Ok(axum::extract::Query(query))
            if query.country.len() == 2
                && query.country.bytes().all(|byte| byte.is_ascii_alphabetic()) =>
        {
            query.country.to_ascii_uppercase()
        }
        _ => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid(
                    "validation_failed",
                    "`country` is a two-letter regulatory domain, e.g. ?country=GR".to_string(),
                )
                .at(WIFI_AP_PATH),
            );
        }
    };
    let capabilities = radio_capabilities(&state).await;
    let present: Option<Vec<u64>> = capabilities
        .as_ref()
        .and_then(|found| found.get("channels"))
        .and_then(Value::as_array)
        .map(|channels| channels.iter().filter_map(Value::as_u64).collect());
    let channels = micad_settings::ap_channels(&country)
        .filter(|channel| {
            present
                .as_ref()
                .is_none_or(|present| present.contains(&u64::from(*channel)))
        })
        .collect();
    api_response(
        StatusCode::OK,
        WifiChannels {
            country_code: country,
            channels,
            auto: capabilities
                .as_ref()
                .and_then(|found| found.get("acs"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
    )
}
