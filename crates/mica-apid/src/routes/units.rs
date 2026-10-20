//! The user's own services: declared in `services.units`, kept by micad with
//! Base's `mica-unit`, and driven through the init.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// One service as it is declared: what `services.units.<name>` holds.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitBody {
    /// `systemd` or `openrc`: the product's init, which `GET /api/v1/units`
    /// names.
    pub(super) kind: String,
    /// The unit file, or the openrc-run script.
    pub(super) text: String,
    /// The script's `conf.d` settings (OpenRC only).
    #[serde(default)]
    pub(super) conf: Option<String>,
    /// Whether it starts at boot; it is started when it becomes so and
    /// stopped when it stops being so.
    pub(super) enabled: bool,
}

/// The settings dot-path of one service: its name quoted, for its dots.
fn unit_path(name: &str) -> String {
    format!(
        "{UNITS_SETTINGS_PATH}.{}",
        micad_settings::quote_path_segment(name)
    )
}

/// Every service declared or put there with `mica-unit`.
#[utoipa::path(
    get,
    path = V1_UNITS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`init`, whether this root has `mica-unit` (`available`), and `units`: each one declared or in `mica-unit`'s directory, with `declared`, `managed` (micad applies it), `installed`, `enabled`, the init's `active` state, and `refused` for one declared for the other init", body = Object),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_units_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_units().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(UNITS_SETTINGS_PATH)),
    }
}

/// Declare or replace one service.
///
/// micad refuses a name of the wrong form for the init, a unit of the other
/// init, a name (or an `Alias=`) the root ships, an enabled unit nothing
/// would start, and a root with no `mica-unit`; nothing is written then.
#[utoipa::path(
    put,
    path = V1_UNIT_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The unit file's name (systemd: `x.service`, `x.timer`, ...), or the script's (OpenRC)")),
    request_body = UnitBody,
    responses(
        (status = 202, description = "The service was declared and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "A body that is not `{kind, text, conf?, enabled}` (`validation_failed`); a service micad refuses (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`, `settings_io`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_unit_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body: UnitBody = match json_body(body, Some(UNITS_SETTINGS_PATH)) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let path = unit_path(&name);
    let mut value = serde_json::json!({
        "kind": body.kind,
        "text": body.text,
        "enabled": body.enabled,
    });
    if let Some(conf) = body.conf {
        value["conf"] = Value::String(conf);
    }
    match state.api.set_settings(&path, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(&path)),
    }
}

/// Remove one declared service: micad stops it and deletes it with its
/// links.
#[utoipa::path(
    delete,
    path = V1_UNIT_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The declared service to remove")),
    responses(
        (status = 202, description = "The service was removed and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No service of this name is declared (`not_found`)", body = ApiError),
        (status = 500, description = "The stored services could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_unit_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let mut units = match stored_map(&state, UNITS_SETTINGS_PATH).await {
        Ok(units) => units,
        Err(response) => return *response,
    };
    if units.remove(&name).is_none() {
        return item_not_found(UNITS_SETTINGS_PATH, &name);
    }
    match state
        .api
        .set_settings(UNITS_SETTINGS_PATH, &Value::Object(units))
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(UNITS_SETTINGS_PATH)),
    }
}

/// Start, stop or restart an installed service, managed by micad or put
/// there with `mica-unit`.
///
/// What it does lasts until the next boot or the next change of `enabled`.
#[utoipa::path(
    post,
    path = V1_UNIT_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("name" = String, Path, description = "The installed service"),
        ("action" = String, Path, description = "`start`, `stop` or `restart`"),
    ),
    responses(
        (status = 204, description = "The init carried out the verb"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "No such service is installed (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The init refused the verb (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_unit_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    // An allowlist and not a passthrough, as the container verbs are.
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return item_not_found(UNITS_SETTINGS_PATH, &action);
    }
    match state.api.unit_action(&name, &action).await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(UNITS_SETTINGS_PATH)),
    }
}

/// The newest lines of an installed service's log: its journal, or on
/// OpenRC the RAM log's lines tagged with its name. Each line is scrubbed as
/// a service log's are before it leaves the device.
#[utoipa::path(
    get,
    path = V1_UNIT_LOG_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The installed service")),
    responses(
        (status = 200, description = "`source`, `available`, and when available the scrubbed `lines` (at most 200), oldest first, and `truncated`; otherwise `detail`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 422, description = "No such service is installed (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_unit_log(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    match state.api.unit_log(&name).await {
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
        Err(err) => bus_api_error(&err, Some(UNITS_SETTINGS_PATH)),
    }
}
