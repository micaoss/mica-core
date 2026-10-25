//! LAN discovery over SSDP: what the responder announces, as micad's `ssdp`
//! reconciler reports it, and the UPnP device description its `LOCATION`
//! points at. Its switch is `access.ssdp.enabled`, written through
//! `PUT /api/v1/settings/...`.

use axum::extract::{Request, State};
use axum::http::header::{CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::*;

/// The console the description's `presentationURL` names.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct SsdpConsole {
    /// `http`, or `https` while HTTPS is on.
    pub(super) scheme: String,
    pub(super) port: u16,
}

/// The responder as the reconciler last reported it.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SsdpState {
    /// Whether the switch is on.
    pub(super) enabled: bool,
    /// What decided it: `setting` (`access.ssdp.enabled`) or `product`.
    pub(super) source: String,
    /// Whether the responder runs: on, with the console on the network.
    pub(super) active: bool,
    /// Why it does not run while on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
    /// The device's UDN, `uuid:...`, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) udn: Option<String>,
    /// An opaque, stable serial number derived from the machine id, while it
    /// runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) serial_number: Option<String>,
    /// The hostname.
    pub(super) friendly_name: String,
    pub(super) manufacturer: String,
    /// The product.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) model_name: Option<String>,
    pub(super) device_type: String,
    pub(super) console: SsdpConsole,
}

/// The responder's report, or why there is none.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SsdpOverview {
    /// Whether the reconciler reported a state.
    pub(super) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) state: Option<SsdpState>,
    /// Why there is no state: none published, or the last apply failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// The reconciler's report, or why there is none.
async fn ssdp_state(state: &AppState) -> Result<SsdpState, String> {
    let value = match state.api.get_state(SSDP_STATE_PATH).await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "live SSDP state unavailable");
            return Err("the SSDP reconciler has published no state".to_string());
        }
    };
    if let Some(error) = value["error"]
        .as_str()
        .or_else(|| value["refused"].as_str())
    {
        return Err(error.to_string());
    }
    serde_json::from_value(value).map_err(|err| format!("the SSDP state could not be read: {err}"))
}

/// Whether the device answers UPnP discovery, and what it announces.
///
/// Read-only: the switch is `access.ssdp.enabled`.
#[utoipa::path(
    get,
    path = V1_SSDP_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The responder's last reported state, or why there is none", body = SsdpOverview),
        (status = 401, description = "No API credential was supplied", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ssdp_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let overview = match ssdp_state(&state).await {
        Ok(reported) => SsdpOverview {
            available: true,
            state: Some(reported),
            error: None,
        },
        Err(error) => SsdpOverview {
            available: false,
            state: None,
            error: Some(error),
        },
    };
    api_response(StatusCode::OK, overview)
}

/// The UPnP device description at `/ssdp/device.xml`, without a credential
/// and on the HTTP listener too, where `LOCATION` points: the hostname, Mica,
/// the product, the opaque serial number, the UDN and the console's URL on
/// the address asked. 404 while the responder does not run.
pub(super) async fn ssdp_description(State(state): State<AppState>, request: Request) -> Response {
    let Ok(reported) = ssdp_state(&state).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match description(&reported, request.headers()) {
        Some(xml) => (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
            xml,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The description of `reported` as asked with `headers`; none while the
/// responder does not run.
pub(super) fn description(reported: &SsdpState, headers: &HeaderMap) -> Option<String> {
    let (true, Some(udn), Some(serial)) = (
        reported.active,
        reported.udn.as_deref(),
        reported.serial_number.as_deref(),
    ) else {
        return None;
    };
    let mut device = format!(
        "<deviceType>{}</deviceType><friendlyName>{}</friendlyName><manufacturer>{}</manufacturer>",
        escape(&reported.device_type),
        escape(&reported.friendly_name),
        escape(&reported.manufacturer),
    );
    if let Some(model) = &reported.model_name {
        device.push_str(&format!("<modelName>{}</modelName>", escape(model)));
    }
    device.push_str(&format!(
        "<serialNumber>{}</serialNumber><UDN>{}</UDN>",
        escape(serial),
        escape(udn)
    ));
    if let Some(host) = asked_host(headers) {
        let console = &reported.console;
        device.push_str(&format!(
            "<presentationURL>{}://{}:{}/</presentationURL>",
            escape(&console.scheme),
            escape(host),
            console.port
        ));
    }
    Some(format!(
        "<?xml version=\"1.0\"?>\n<root xmlns=\"urn:schemas-upnp-org:device-1-0\" configId=\"1\"><specVersion><major>1</major><minor>1</minor></specVersion><device>{device}</device></root>\n"
    ))
}

/// The host the description was asked of, without its port: a name, an IPv4
/// address or a bracketed IPv6 one.
fn asked_host(headers: &HeaderMap) -> Option<&str> {
    let host = headers.get(HOST)?.to_str().ok()?;
    let host = match host.rsplit_once(':') {
        Some((name, port))
            if !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && (!name.contains(':') || name.ends_with(']')) =>
        {
            name
        }
        _ => host,
    };
    (!host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']')))
    .then_some(host)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
