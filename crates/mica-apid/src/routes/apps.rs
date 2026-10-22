//! The user's apps: bundles micad installs under `/mica/apps`, their units and
//! their data. Every change is a micad task of kind `app.apply`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The largest bundle micad accepts.
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

/// One rule a refused bundle breaks.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct AppRule {
    /// `path_outside`, `link_outside`, `special_file`, `setuid`,
    /// `unit_shipped_by_root`, `unit_taken`, `arch`, `init`, `manifest`,
    /// `too_large` or `too_many`.
    pub(super) rule: String,
    /// What broke it.
    pub(super) detail: String,
}

/// How an upload is installed.
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct InstallQuery {
    /// Install the version that is installed again.
    #[serde(default)]
    pub(super) reinstall: bool,
    /// Accept the privileges the bundle asks for beyond the installed
    /// version's; without it such a bundle is refused (`app_privileges`).
    #[serde(default, rename = "acceptPrivileges")]
    #[param(rename = "acceptPrivileges")]
    pub(super) accept_privileges: bool,
}

/// Which version an update takes asking for more privileges.
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct ActionQuery {
    /// `update` only: the version whose privileges beyond the installed
    /// one's are accepted, as `update.privileges` showed them. A different
    /// version or bundle, or one asking more, is asked about again.
    #[serde(default, rename = "acceptPrivileges")]
    #[param(rename = "acceptPrivileges")]
    pub(super) accept_privileges: String,
}

/// Whether a removal deletes the app's data.
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct RemoveQuery {
    /// Delete `/srv/apps/<name>` too.
    #[serde(default)]
    pub(super) purge: bool,
}

/// Map an app-surface bus failure, reading the names only micad's app
/// members raise before handing the rest to the shared mapping.
fn app_bus_error(err: &anyhow::Error) -> Response {
    if let Some((name, message)) = err.downcast_ref::<zbus::Error>().and_then(error_reply) {
        let message = message.unwrap_or_else(|| name.clone());
        let (status, error) = match name.as_str() {
            MICAD_APP_INVALID => {
                #[derive(serde::Deserialize)]
                struct Invalid {
                    message: String,
                    details: Vec<AppRule>,
                }
                let invalid: Option<Invalid> = serde_json::from_str(&message).ok();
                let (message, rules) =
                    invalid.map_or((message, Vec::new()), |i| (i.message, i.details));
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    ApiError::micad("app_invalid", message).with_rules(rules),
                )
            }
            MICAD_APP_PRIVILEGES => {
                let asked: Value = serde_json::from_str(&message).unwrap_or(Value::Null);
                (
                    StatusCode::CONFLICT,
                    ApiError::micad(
                        "app_privileges",
                        asked["message"].as_str().unwrap_or(&message).to_string(),
                    )
                    .with_params(asked["privileges"].clone()),
                )
            }
            MICAD_APP_BUSY => (StatusCode::CONFLICT, ApiError::micad("app_busy", message)),
            MICAD_APP_INSTALLED => (
                StatusCode::CONFLICT,
                ApiError::micad("app_installed", message),
            ),
            MICAD_NOT_FOUND => (
                StatusCode::NOT_FOUND,
                ApiError::micad("app_not_found", message),
            ),
            FDO_INVALID_ARGS => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::micad("app_refused", message),
            ),
            _ => return bus_api_error(err, None),
        };
        tracing::warn!(error = %err, "app call refused");
        return api_response(status, error);
    }
    bus_api_error(err, None)
}

/// Every installed app.
#[utoipa::path(
    get,
    path = V1_APPS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`available` (whether this daemon keeps apps), `arch` and `apps`: each with `name`, `version`, `description`, `arch`, `sha256`, `source` (`upload`, `cli`, `url`, `seed`), `installedAt`, `previous`, `units` (`name`, `enabled`, `active`), `privileges` (`user` `app` or `root`, `account` and `uid` of an app's own user, `capabilities`, `devices`), `last` (`action`, `outcome`, `message`, `at`), `update` (`url`, `auto`, `available`, `checkedAt`, `error`, and `privileges` `{root, capabilities, devices}` the available version asks for beyond the installed one's once an update fetched it) and `seed` (`version`, `policy`)", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_apps_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_apps().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(value)),
        Err(err) => app_bus_error(&err),
    }
}

/// One installed app, as the list shows it.
#[utoipa::path(
    get,
    path = V1_APP_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The app")),
    responses(
        (status = 200, description = "The app, as an entry of `GET /api/v1/apps`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "No app of this name is installed (`app_not_found`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_app_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    match state.api.get_app(&name).await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(value)),
        Err(err) => app_bus_error(&err),
    }
}

/// Install or upgrade an app from its bundle: the manifest's name decides
/// which. micad checks the manifest before it answers and unpacks the rest
/// in the task.
#[utoipa::path(
    post,
    path = V1_APPS_PATH,
    context_path = API,
    tag = "actions",
    params(InstallQuery),
    request_body(content = String, description = "The app bundle, a gzip tar", content_type = "application/octet-stream"),
    responses(
        (status = 202, description = "The bundle was taken and its install queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The upload was interrupted (`app_upload_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Another app change is running (`app_busy`), this version is installed and `reinstall` is not set (`app_installed`), or the bundle asks for privileges beyond the installed version's (any, on a first install) and `acceptPrivileges` is not set (`app_privileges`, with `params` `{root, capabilities, devices}`: what it asks for more; nothing is installed)", body = ApiError),
        (status = 413, description = "The bundle is above 256 MiB (`app_too_large`)", body = ApiError),
        (status = 415, description = "The body is not `application/octet-stream` (`app_upload_type`)", body = ApiError),
        (status = 422, description = "The bundle breaks a rule (`app_invalid`, each in `rules`), or cannot be installed here (`app_refused`)", body = ApiError),
        (status = 507, description = "The upload could not be written (`app_storage_unavailable`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_app_install(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Query(query): Query<InstallQuery>,
    request: axum::extract::Request<axum::body::Body>,
) -> Response {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let refuse = |status: StatusCode, code: &'static str, message: &str| {
        api_response(status, ApiError::apid(code, message.to_string()))
    };
    let too_large = || {
        refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "app_too_large",
            "this device accepts an app bundle of at most 256 MiB",
        )
    };
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type != Some("application/octet-stream") {
        return refuse(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "app_upload_type",
            "upload the bundle as application/octet-stream",
        );
    }
    if request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_BUNDLE_BYTES)
    {
        return too_large();
    }
    let storage = |what: &str| {
        refuse(
            StatusCode::INSUFFICIENT_STORAGE,
            "app_storage_unavailable",
            &format!("the upload could not be {what}"),
        )
    };
    // A name drawn here, never the client's: micad takes the file by the
    // path apid names.
    let upload = state.app_uploads.join(format!(
        "{:032x}.tar.gz",
        u128::from_ne_bytes(micad_settings::random_bytes())
    ));
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&upload)
        .await
    {
        Ok(file) => file,
        Err(err) => {
            tracing::error!(error = %err, "creating the app upload file failed");
            return storage("created");
        }
    };
    let mut stream = request.into_body().into_data_stream();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                let _ = tokio::fs::remove_file(&upload).await;
                tracing::warn!(error = %err, "the app upload was interrupted");
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "app_upload_invalid",
                    "the upload was interrupted",
                );
            }
        };
        written += chunk.len() as u64;
        if written > MAX_BUNDLE_BYTES {
            let _ = tokio::fs::remove_file(&upload).await;
            return too_large();
        }
        if let Err(err) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&upload).await;
            tracing::error!(error = %err, "writing the app upload failed");
            return storage("written");
        }
    }
    if let Err(err) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&upload).await;
        tracing::error!(error = %err, "syncing the app upload failed");
        return storage("written");
    }
    drop(file);
    match state
        .api
        .install_app(
            &upload.to_string_lossy(),
            query.reinstall,
            query.accept_privileges,
        )
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => {
            // Whatever micad did not take is apid's to remove.
            let _ = tokio::fs::remove_file(&upload).await;
            app_bus_error(&err)
        }
    }
}

/// Remove an app: its units are stopped and removed with its versions.
#[utoipa::path(
    delete,
    path = V1_APP_ROUTE,
    context_path = API,
    tag = "actions",
    params(("name" = String, Path, description = "The app"), RemoveQuery),
    responses(
        (status = 202, description = "The removal was queued; the body carries the task id", body = TaskAccepted),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No app of this name is installed (`app_not_found`)", body = ApiError),
        (status = 409, description = "Another app change is running (`app_busy`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_app_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> Response {
    match state.api.remove_app(&name, query.purge).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => app_bus_error(&err),
    }
}

/// One action on an app: `rollback` to its previous version, `check` its
/// update URL or `update` from it, each a task; or `start`, `stop` or
/// `restart` its units.
#[utoipa::path(
    post,
    path = V1_APP_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("name" = String, Path, description = "The app"),
        ("action" = String, Path, description = "`rollback`, `check`, `update`, `start`, `stop` or `restart`"),
        ActionQuery,
    ),
    responses(
        (status = 202, description = "`rollback`, `check` or `update` was queued; the body carries the task id. An `update` to a version asking for privileges beyond the installed one's fails its task unless `acceptPrivileges` names it, and the app's `update.privileges` then says what it asks for", body = TaskAccepted),
        (status = 204, description = "The init carried out `start`, `stop` or `restart`"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No app of this name is installed (`app_not_found`), or the path names no action (`not_found`)", body = ApiError),
        (status = 409, description = "Another app change is running (`app_busy`)", body = ApiError),
        (status = 422, description = "The app cannot do it: no previous version, no update URL, nothing newer (`app_refused`)", body = ApiError),
        (status = 500, description = "The init refused the verb (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_app_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
    Query(query): Query<ActionQuery>,
) -> Response {
    match action.as_str() {
        "rollback" | "check" | "update" => match state
            .api
            .app_flow(&name, &action, &query.accept_privileges)
            .await
        {
            Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
            Err(err) => app_bus_error(&err),
        },
        "start" | "stop" | "restart" => match state.api.app_action(&name, &action).await {
            Ok(()) => no_content(),
            Err(err) => app_bus_error(&err),
        },
        _ => api_response(
            StatusCode::NOT_FOUND,
            ApiError::apid("not_found", format!("an app has no action `{action}`")),
        ),
    }
}

/// An app's newest lines: each unit's log and the last first-run hook's
/// output, every line scrubbed as a service log's is.
#[utoipa::path(
    get,
    path = V1_APP_LOG_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The app")),
    responses(
        (status = 200, description = "`name`, `units` (each with `unit`, `available` and the scrubbed `lines`, or `detail`) and `firstRun` (the scrubbed lines, or null)", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "No app of this name is installed (`app_not_found`)", body = ApiError),
        (status = 500, description = "micad failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_app_log(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let scrub = |lines: Option<&mut Value>| {
        if let Some(lines) = lines.and_then(Value::as_array_mut) {
            for line in lines.iter_mut() {
                if let Some(text) = line.as_str() {
                    *line = Value::String(crate::diagnostics::scrub(text).0);
                }
            }
        }
    };
    match state.api.app_log(&name).await {
        Ok(mut value) => {
            if let Some(units) = value.get_mut("units").and_then(Value::as_array_mut) {
                for unit in units.iter_mut() {
                    scrub(unit.get_mut("lines"));
                }
            }
            scrub(value.get_mut("firstRun"));
            api_response(StatusCode::OK, ResourceValue(value))
        }
        Err(err) => app_bus_error(&err),
    }
}
