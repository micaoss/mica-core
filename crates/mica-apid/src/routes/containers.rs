//! Container projects and their verbs.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// A project's Compose file.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComposeBody {
    /// The Compose file's text, YAML.
    pub(super) compose: String,
}

/// What a project's check found: `ok`, and the `containers` its services
/// become, or the `detail` and every one of the `problems`.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComposeCheck {
    /// Whether mica-containerd runs the file as written.
    pub(super) ok: bool,
    /// The containers the services become, as mica-containerd's specs.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Vec<Object>>)]
    pub(super) containers: Option<Value>,
    /// Every problem, joined with `; `.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
    /// Every problem, by service and field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) problems: Option<Vec<ComposeProblem>>,
}

/// One problem of a refused Compose file.
#[derive(Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct ComposeProblem {
    /// The service it is about; empty for the project as a whole.
    #[serde(default)]
    pub(crate) service: String,
    /// The Compose key it is about.
    #[serde(default)]
    pub(crate) field: String,
    /// What is wrong with it.
    #[serde(default)]
    pub(crate) detail: String,
}

impl ComposeCheck {
    fn of(value: &Value) -> Self {
        let ok = value["ok"] == serde_json::json!(true);
        Self {
            ok,
            containers: ok.then(|| value["containers"].clone()),
            detail: (!ok).then(|| {
                value["detail"]
                    .as_str()
                    .unwrap_or("the Compose file is refused")
                    .to_string()
            }),
            problems: (!ok)
                .then(|| serde_json::from_value(value["problems"].clone()).unwrap_or_default()),
        }
    }
}

/// A container's log.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ContainerLog {
    /// The last lines, scrubbed, oldest first.
    pub(super) log: String,
}

/// How many lines of a log.
#[derive(serde::Deserialize, utoipa::IntoParams)]
pub(crate) struct LogQuery {
    /// 1 to 1000; 200 when absent.
    pub(super) tail: Option<u32>,
}

/// The projects, and what mica-containerd reports, as micad joins them.
#[utoipa::path(
    get,
    path = V1_CONTAINERS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`enabled`, the `projects` (each name to its Compose text), `projectDirs` (each project declared with a directory to it), mica-containerd's `engine` read (`projects` with each service's container, phase, health, restarts and exit code) and podman's `images`, each with its availability", body = Object),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_containers_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_containers().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(CONTAINER_PROJECTS_SETTINGS_PATH)),
    }
}

/// The settings dot-path of one project.
fn project_path(project: &str) -> String {
    format!("{CONTAINER_PROJECTS_SETTINGS_PATH}.{project}")
}

/// Refuse a name that is not a project's before micad is asked anything.
fn require_project_name(project: &str) -> Result<(), Box<Response>> {
    if micad_settings::is_project_name(project) {
        return Ok(());
    }
    Err(Box::new(api_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::apid(
            "validation_failed",
            format!(
                "{project:?} is not a project name: 1 to 40 of a-z, 0-9, `_` and `-`, starting with a letter or a digit"
            ),
        )
        .at(CONTAINER_PROJECTS_SETTINGS_PATH),
    )))
}

/// The check of `compose` as `project`, or the response saying why it could
/// not run.
async fn check(
    state: &AppState,
    project: &str,
    compose: &str,
) -> Result<ComposeCheck, Box<Response>> {
    state
        .api
        .check_container_project(project, compose)
        .await
        .map(|value| ComposeCheck::of(&value))
        .map_err(|err| Box::new(bus_api_error(&err, Some(CONTAINER_PROJECTS_SETTINGS_PATH))))
}

/// Declare or replace one project.
///
/// The file is checked first, as mica-containerd checks it, so a refusal
/// names every problem by service and field and nothing is written. A
/// replaced project keeps its directory.
#[utoipa::path(
    put,
    path = V1_CONTAINER_PROJECT_ROUTE,
    context_path = API,
    tag = "resources",
    params(("project" = String, Path, description = "The project to declare or replace")),
    request_body = ComposeBody,
    responses(
        (status = 202, description = "The project was declared and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "A name that is not a project's, or a body that is not `{\"compose\"}` (`validation_failed`); a Compose file mica-containerd refuses, with every problem in `problems` (`compose_invalid`); a set micad refuses (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The check could not run, or micad failed (`micad_failed`, `settings_io`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_project_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(project): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_project_name(&project) {
        return *response;
    }
    let body: ComposeBody = match json_body(body, Some(CONTAINER_PROJECTS_SETTINGS_PATH)) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let checked = match check(&state, &project, &body.compose).await {
        Ok(checked) => checked,
        Err(response) => return *response,
    };
    if !checked.ok {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "compose_invalid",
                format!(
                    "project {project} is refused: {}",
                    checked.detail.unwrap_or_default()
                ),
            )
            .at(&project_path(&project))
            .with_problems(checked.problems.unwrap_or_default()),
        );
    }
    let path = project_path(&project);
    match state
        .api
        .set_settings(&path, &Value::String(body.compose))
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(&path)),
    }
}

/// Remove one project: its containers and network go; `/mica/volumes/<project>/`
/// stays.
#[utoipa::path(
    delete,
    path = V1_CONTAINER_PROJECT_ROUTE,
    context_path = API,
    tag = "resources",
    params(("project" = String, Path, description = "The declared project to remove")),
    responses(
        (status = 202, description = "The project was removed and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No project of this name is declared (`not_found`)", body = ApiError),
        (status = 500, description = "The stored projects could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_project_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(project): Path<String>,
) -> Response {
    let mut projects = match stored_map(&state, CONTAINER_PROJECTS_SETTINGS_PATH).await {
        Ok(projects) => projects,
        Err(response) => return *response,
    };
    if projects.remove(&project).is_none() {
        return item_not_found(CONTAINER_PROJECTS_SETTINGS_PATH, &project);
    }
    match state
        .api
        .set_settings(CONTAINER_PROJECTS_SETTINGS_PATH, &Value::Object(projects))
        .await
    {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(CONTAINER_PROJECTS_SETTINGS_PATH)),
    }
}

/// Check a Compose file as a project, storing nothing.
#[utoipa::path(
    post,
    path = V1_CONTAINER_PROJECT_CHECK_ROUTE,
    context_path = API,
    tag = "actions",
    params(("project" = String, Path, description = "The project the file is checked as")),
    request_body = ComposeBody,
    responses(
        (status = 200, description = "What the check found: the containers, or every problem", body = ComposeCheck),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "A name that is not a project's, or a body that is not `{\"compose\"}` (`validation_failed`); a file larger than a project may be (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The check could not run (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_project_check(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(project): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(response) = require_project_name(&project) {
        return *response;
    }
    let body: ComposeBody = match json_body(body, Some(CONTAINER_PROJECTS_SETTINGS_PATH)) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    match check(&state, &project, &body.compose).await {
        Ok(checked) => api_response(StatusCode::OK, checked),
        Err(response) => *response,
    }
}

/// One verb on what `name` stands for, after the action is held to the three.
async fn verb(state: &AppState, name: &str, action: &str) -> Response {
    // An allowlist and not a passthrough: the action reaches micad as a verb,
    // and a path segment that is not one of these names nothing.
    if !matches!(action, "start" | "stop" | "restart") {
        return item_not_found(CONTAINER_PROJECTS_SETTINGS_PATH, action);
    }
    match state.api.container_action(name, action).await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(CONTAINER_PROJECTS_SETTINGS_PATH)),
    }
}

/// Start, stop or restart every container of a project.
///
/// The verb is mica-containerd's, which keeps it as each container's desired
/// state: a stop holds until a start, a restart of the daemon included.
#[utoipa::path(
    post,
    path = V1_CONTAINER_PROJECT_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("project" = String, Path, description = "The declared project"),
        ("action" = String, Path, description = "`start`, `stop` or `restart`"),
    ),
    responses(
        (status = 204, description = "mica-containerd accepted the verb"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "micad holds no such project or service (`settings_rejected`)", body = ApiError),
        (status = 500, description = "mica-containerd refused the verb or did not answer, or containers are off (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_project_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((project, action)): Path<(String, String)>,
) -> Response {
    verb(&state, &project, &action).await
}

/// Start, stop or restart one service's container.
#[utoipa::path(
    post,
    path = V1_CONTAINER_SERVICE_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("project" = String, Path, description = "The declared project"),
        ("service" = String, Path, description = "One of its services"),
        ("action" = String, Path, description = "`start`, `stop` or `restart`"),
    ),
    responses(
        (status = 204, description = "mica-containerd accepted the verb"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "micad holds no such project or service (`settings_rejected`)", body = ApiError),
        (status = 500, description = "mica-containerd refused the verb or did not answer, or containers are off (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_service_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((project, service, action)): Path<(String, String, String)>,
) -> Response {
    verb(&state, &format!("{project}/{service}"), &action).await
}

/// The last lines of one service's log, bounded, each line scrubbed as a
/// service log's are before it leaves the device.
#[utoipa::path(
    get,
    path = V1_CONTAINER_SERVICE_LOG_ROUTE,
    context_path = API,
    tag = "resources",
    params(
        ("project" = String, Path, description = "The declared project"),
        ("service" = String, Path, description = "One of its services"),
        LogQuery,
    ),
    responses(
        (status = 200, description = "The last `tail` lines, at most 256 KiB, the newest kept", body = ContainerLog),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 422, description = "No such project or service, or a `tail` outside 1 to 1000 (`settings_rejected`)", body = ApiError),
        (status = 500, description = "mica-containerd did not answer, or containers are off (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_service_log(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((project, service)): Path<(String, String)>,
    Query(query): Query<LogQuery>,
) -> Response {
    let name = format!("{project}/{service}");
    match state
        .api
        .container_log(&name, query.tail.unwrap_or(DEFAULT_LOG_TAIL))
        .await
    {
        Ok(log) => {
            let log: Vec<String> = log
                .lines()
                .map(|line| crate::diagnostics::scrub(line).0)
                .collect();
            let mut log = log.join("\n");
            if !log.is_empty() {
                log.push('\n');
            }
            api_response(StatusCode::OK, ContainerLog { log })
        }
        Err(err) => bus_api_error(&err, Some(CONTAINER_PROJECTS_SETTINGS_PATH)),
    }
}

/// The lines a log read answers unless asked.
const DEFAULT_LOG_TAIL: u32 = 200;

/// A settings map, or the envelope saying why it could not be read. An absent
/// map is an empty one: the settings skip an empty map.
async fn stored_map(
    state: &AppState,
    path: &str,
) -> Result<serde_json::Map<String, Value>, Box<Response>> {
    match state.api.get_settings(path).await {
        Ok(Value::Object(map)) => Ok(map),
        Ok(Value::Null) => Ok(serde_json::Map::new()),
        Ok(_) => Err(Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored {path} is not a map"),
            )
            .at(path),
        ))),
        Err(err) => Err(Box::new(bus_api_error(&err, Some(path)))),
    }
}
