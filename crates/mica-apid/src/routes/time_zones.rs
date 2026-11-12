//! The zone catalog (`GET /api/v1/time/zones`): the zones this device's
//! tzdata offers, read from its own tree, and where the stored zone stands
//! against them. The zone is still written as the `time.timezone` setting.

use std::hash::{Hash, Hasher};

use axum::extract::State;
use axum::http::header::{ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use micad_settings::timezones::{zone_catalog, zone_installed};
use serde_json::json;

use super::*;

/// Read the zone catalog.
///
/// `UTC` first, then the canonical zones of the device's tzdata
/// (`zone1970.tab`), each `{id, region, city}` split from its IANA name, with
/// the tzdata release as `tzdataVersion`. `current` is the stored
/// `time.timezone`: whether the device's tzdata carries it (`installed`) and
/// whether it is one of the listed zones (`listed`); an installed zone that
/// is not listed is a backward-compatible name the device still accepts.
/// Offsets are not part of the answer: they depend on the date, and a client
/// computes them. Carries an `ETag`; a request whose `If-None-Match` names it
/// is answered 304.
#[utoipa::path(
    get,
    path = V1_TIME_ZONES_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`tzdataVersion`, `zones` and `current`", body = ResourceValue),
        (status = 304, description = "Unchanged since the `ETag` the request names"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 503, description = "The device's tzdata has no zone table (`tzdata_unavailable`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_time_zones(
    _credential: ApiCredential,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let zoneinfo = Arc::clone(&state.zoneinfo);
    let current = state
        .api
        .get_settings("time.timezone")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_string));
    let read = tokio::task::spawn_blocking(move || {
        zone_catalog(&zoneinfo).map(|catalog| {
            let current = current.map(|id| {
                json!({
                    "installed": zone_installed(&zoneinfo, &id),
                    "listed": catalog.zones.iter().any(|zone| zone.id == id),
                    "id": id,
                })
            });
            (catalog, current)
        })
    })
    .await;
    let Ok(Ok((catalog, current))) = read else {
        return api_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError::apid(
                "tzdata_unavailable",
                "this device's tzdata has no zone table".to_string(),
            ),
        );
    };
    let body = json!({
        "tzdataVersion": catalog.tzdata_version,
        "zones": catalog.zones,
        "current": current,
    });
    let mut hasher = std::hash::DefaultHasher::new();
    body.to_string().hash(&mut hasher);
    let etag = format!("\"{:016x}\"", hasher.finish());
    let tag = HeaderValue::from_str(&etag).ok();
    if headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        if let Some(tag) = tag {
            response.headers_mut().insert(ETAG, tag);
        }
        return response;
    }
    let mut response = api_response(StatusCode::OK, ResourceValue(body));
    if let Some(tag) = tag {
        response.headers_mut().insert(ETAG, tag);
    }
    response
}
