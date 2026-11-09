//! One service's log, read-only and bounded, for the console.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;
use crate::settings_api::LogBoot;

/// Which boot's log.
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct LogBootQuery {
    /// `current` (the default) or `previous`.
    pub(super) boot: Option<String>,
}

/// Read the newest lines of one service's log.
///
/// micad reads at most 200 lines of the current boot, or with
/// `boot=previous` of the one before it, from the allowlisted
/// service (`micad`, `apid`, `time`, `ssh`, `wifi-client`, `wifi-ap`,
/// `bluetooth`, `mqtt-broker`, `mqttd`; a feature the product leaves out has
/// no log), bounded in bytes and time. Every line is scrubbed here before it
/// leaves the device, as a diagnostic snapshot's are: a line carrying a secret
/// marker is replaced whole, and a hardware address is masked. A log that
/// keeps no previous boot answers `available: false` with the reason.
#[utoipa::path(
    get,
    path = V1_SYSTEM_LOG_ROUTE,
    context_path = API,
    tag = "resources",
    params(
        ("source" = String, Path, description = "The service whose log to read"),
        LogBootQuery,
    ),
    responses(
        (status = 200, description = "`source`, `available`, and when available the scrubbed `lines`, oldest first, and `truncated`; otherwise `detail`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 422, description = "No such log, one of a feature this product leaves out, or a `boot` other than `current` or `previous` (`validation_failed`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_system_log(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(source): Path<String>,
    Query(query): Query<LogBootQuery>,
) -> Response {
    let boot = match query.boot.as_deref() {
        None | Some("current") => LogBoot::Current,
        Some("previous") => LogBoot::Previous,
        Some(other) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid(
                    "validation_failed",
                    format!(
                        "no boot named {other:?}; a log is of the `current` or the `previous` boot"
                    ),
                ),
            );
        }
    };
    match state.api.get_log(&source, boot).await {
        Ok(mut value) => {
            if let Some(lines) = value.get_mut("lines").and_then(Value::as_array_mut) {
                for line in lines.iter_mut() {
                    if let Some(text) = line.as_str() {
                        *line = Value::String(crate::diagnostics::scrub(text).0);
                    }
                }
            }
            api_response(StatusCode::OK, ResourceValue(value))
        }
        Err(err) => bus_api_error(&err, None),
    }
}
