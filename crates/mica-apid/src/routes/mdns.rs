//! LAN discovery over mDNS: what the responder publishes, as micad's `mdns`
//! reconciler reports it. Its switch is `access.mdns.enabled`, written
//! through `PUT /api/v1/settings/...`.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use super::*;

/// One service the responder publishes, or would while it is off.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MdnsService {
    /// The instance label, such as the hostname.
    pub(super) instance: String,
    /// The DNS-SD type, `_<name>._tcp` or `_<name>._udp`.
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) port: u16,
    pub(super) txt: BTreeMap<String, String>,
    /// `built-in`, `image:<name>`, `runtime:<name>` or `app:<name>`.
    pub(super) source: String,
    /// Whether it is on the wire now: false while the responder is off.
    pub(super) published: bool,
}

/// A drop-in file or an app entry the responder refused.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct MdnsRefusal {
    pub(super) source: String,
    pub(super) error: String,
}

/// The responder as the reconciler last reported it.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MdnsState {
    /// Whether the responder runs.
    pub(super) enabled: bool,
    /// What decided it: `setting` (`access.mdns.enabled`) or `product`.
    pub(super) source: String,
    /// The name it answers, `<hostname>.local`.
    pub(super) hostname: String,
    pub(super) services: Vec<MdnsService>,
    #[serde(default)]
    pub(super) refused: Vec<MdnsRefusal>,
}

/// The responder's report, or why there is none.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MdnsOverview {
    /// Whether the reconciler reported a state.
    pub(super) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) state: Option<MdnsState>,
    /// Why there is no state: none published, or the last apply failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// What the device announces on the LAN over mDNS.
///
/// Read-only: the switch is `access.mdns.enabled`; services come from the
/// device, the image's and the running system's drop-in files and the
/// running apps.
#[utoipa::path(
    get,
    path = V1_MDNS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The responder's last reported state, or why there is none", body = MdnsOverview),
        (status = 401, description = "No API credential was supplied", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_mdns_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let overview = match state.api.get_state(MDNS_STATE_PATH).await {
        Ok(value) => {
            let failed = value["error"]
                .as_str()
                .or_else(|| value["refused"].as_str())
                .map(str::to_string);
            match (failed, serde_json::from_value::<MdnsState>(value)) {
                (None, Ok(reported)) => MdnsOverview {
                    available: true,
                    state: Some(reported),
                    error: None,
                },
                (Some(error), _) => MdnsOverview {
                    available: false,
                    state: None,
                    error: Some(error),
                },
                (None, Err(err)) => MdnsOverview {
                    available: false,
                    state: None,
                    error: Some(format!("the mDNS state could not be read: {err}")),
                },
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "live mDNS state unavailable");
            MdnsOverview {
                available: false,
                state: None,
                error: Some("the mDNS reconciler has published no state".to_string()),
            }
        }
    };
    api_response(StatusCode::OK, overview)
}
