//! Container declarations and actions.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// One declared container, documented.
///
/// A mirror of `micad_settings::ContainerUnit`, held field-for-field against
/// it by a test, for the reason `NetworkInterface` is a mirror: the model is
/// what the body deserializes into, and this is what says so in the document.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContainerDeclaration {
    /// The image reference, tag included.
    pub(super) image: String,
    /// The command to run instead of the image's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) command: Vec<String>,
    /// Environment variables passed into the container.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(super) environment: std::collections::BTreeMap<String, String>,
    /// Ports published from the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) publish: Vec<ContainerPort>,
    /// Host paths mounted into the container. A host path is under `/mica/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) volumes: Vec<ContainerVolume>,
    /// `no`, `on-failure` or `always`.
    pub(super) restart: String,
    /// Whether the container starts at boot.
    pub(super) auto_start: bool,
    /// The most processes the container may run, 1 to 65536. Absent is
    /// podman's own ceiling, 2048 -- not unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pids: Option<u32>,
    /// The memory the container may use, `<n>k`, `<n>m` or `<n>g`, at least
    /// `6m`. Absent is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) memory: Option<String>,
    /// The CPUs the container may use, such as `0.5` or `2`, at most three
    /// decimals. Absent is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cpu: Option<String>,
}

/// One published port.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContainerPort {
    /// The port on the host.
    pub(super) host: u16,
    /// The port inside the container.
    pub(super) container: u16,
    /// `tcp` or `udp`.
    pub(super) protocol: String,
}

/// One bind mount.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ContainerVolume {
    /// The path on the device, under `/mica/`.
    pub(super) host: String,
    /// Where it appears inside the container.
    pub(super) container: String,
    /// Whether the container sees it read-only.
    pub(super) read_only: bool,
}

/// The declared containers and what mica-containerd reports, as micad joins them.
#[utoipa::path(
    get,
    path = V1_CONTAINERS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`enabled`, the declared map, mica-containerd's `engine` read (each container's phase, health, restarts and exit code) and podman's `images`, each with its availability", body = Object),
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
        Err(err) => bus_api_error(&err, Some(CONTAINERS_SETTINGS_PATH)),
    }
}

/// The declared containers, by name, as the settings tree holds them.
pub(super) type ContainerMap = std::collections::BTreeMap<String, micad_settings::ContainerUnit>;

/// Read the declared map, or the envelope saying why it could not be read.
pub(super) async fn stored_containers(state: &AppState) -> Result<ContainerMap, Box<Response>> {
    let value = match state.api.get_settings(CONTAINERS_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => {
            return Err(Box::new(bus_api_error(
                &err,
                Some(CONTAINERS_SETTINGS_PATH),
            )));
        }
    };
    // An absent map is an empty one: `container.units` is skipped when empty,
    // so a device that declares none has no key here at all.
    if value.is_null() {
        return Ok(ContainerMap::new());
    }
    serde_json::from_value(value).map_err(|err| {
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored container map could not be read: {err}"),
            )
            .at(CONTAINERS_SETTINGS_PATH),
        ))
    })
}

/// Write the map, refusing what the device would refuse.
pub(super) async fn write_containers(
    state: &AppState,
    map: &ContainerMap,
) -> Result<String, Box<Response>> {
    // The same validator the settings tree runs, called here so the operator
    // gets the sentence rather than a bus error carrying it.
    if let Err(message) = micad_settings::validate_container_units(map) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(CONTAINERS_SETTINGS_PATH),
        )));
    }
    let value = encode(map)?;
    state
        .api
        .set_settings(CONTAINERS_SETTINGS_PATH, &value)
        .await
        .map_err(|err| Box::new(bus_api_error(&err, Some(CONTAINERS_SETTINGS_PATH))))
}

/// Declare or replace one container, validated against the whole map.
///
/// **A `PUT` replaces the entry entirely.** Creating and replacing are one
/// operation, so there is no 404 here: a name that is not declared becomes
/// one that is.
#[utoipa::path(
    put,
    path = V1_CONTAINER_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The container to declare or replace")),
    request_body = ContainerDeclaration,
    responses(
        (status = 202, description = "The container was declared and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not a container, or the resulting map breaks a rule -- a name a unit cannot carry, no image, one host port published twice, or a volume outside `/mica/` (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored map could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let unit: micad_settings::ContainerUnit = match json_body(body, Some(CONTAINERS_SETTINGS_PATH))
    {
        Ok(unit) => unit,
        Err(response) => return *response,
    };
    let mut map = match stored_containers(&state).await {
        Ok(map) => map,
        Err(response) => return *response,
    };
    map.insert(name, unit);
    match write_containers(&state, &map).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(response) => *response,
    }
}

/// Remove one container, re-validating the rest of the map without it.
///
/// **The volumes are not deleted with it.** A container's data under `/mica/`
/// outlives the declaration, which is what makes removing and re-declaring a
/// container a safe way to change its image.
#[utoipa::path(
    delete,
    path = V1_CONTAINER_ROUTE,
    context_path = API,
    tag = "resources",
    params(("name" = String, Path, description = "The declared container to remove")),
    responses(
        (status = 202, description = "The container was removed and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No container of this name is declared (`not_found`)", body = ApiError),
        (status = 500, description = "The stored map could not be read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let mut map = match stored_containers(&state).await {
        Ok(map) => map,
        Err(response) => return *response,
    };
    if map.remove(&name).is_none() {
        return item_not_found(CONTAINERS_SETTINGS_PATH, &name);
    }
    match write_containers(&state, &map).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(response) => *response,
    }
}

/// Start, stop or restart one declared container.
///
/// The verb is mica-containerd's, which keeps it as the container's desired
/// state. micad refuses a name it does not hold, so this is not a way to drive
/// an arbitrary container through a declared-container path.
#[utoipa::path(
    post,
    path = V1_CONTAINER_ACTION_ROUTE,
    context_path = API,
    tag = "actions",
    params(
        ("name" = String, Path, description = "The declared container"),
        ("action" = String, Path, description = "`start`, `stop` or `restart`"),
    ),
    responses(
        (status = 204, description = "mica-containerd accepted the verb"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The path names no action this route serves (`not_found`)", body = ApiError),
        (status = 422, description = "micad holds no container of this name (`settings_rejected`)", body = ApiError),
        (status = 500, description = "mica-containerd refused the verb or did not answer (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_container_action(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    // An allowlist and not a passthrough: the action reaches micad as a verb,
    // and a path segment that is not one of these names nothing.
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return item_not_found(CONTAINERS_SETTINGS_PATH, &action);
    }
    match state.api.container_action(&name, &action).await {
        Ok(()) => no_content(),
        Err(err) => bus_api_error(&err, Some(CONTAINERS_SETTINGS_PATH)),
    }
}
