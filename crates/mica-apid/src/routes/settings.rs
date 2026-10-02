//! The settings and live-state trees, and apply tasks.

use crate::audit::Source;
use crate::redact;
use crate::task_registry::TaskRecord;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The body of a resource `GET`: the value at the dot-path, as micad holds it.
///
// `privateKey` is on the redaction list as a fail-closed guard: no shipped
// schema has such a field yet.
/// Any JSON value: a dot-path can name a subtree, an array or a scalar.
///
/// Secrets are replaced by the string `"<redacted>"`, which a client can
/// receive anywhere inside the body. Every field named `psk`, `passwordHash`,
/// `password_hash`, `hash` or `privateKey` carries the sentinel instead of its
/// value, at any depth and inside arrays, and so does the whole body when the
/// dot-path names one of those fields directly.
///
/// The sentinel is read-only: writing it back is refused at **422** rather
/// than stored, because storing it would destroy the credential.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(transparent)]
pub(crate) struct ResourceValue(pub(super) Value);

/// A persisted settings write whose reconciliation continues asynchronously.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskAccepted {
    /// Poll this id at `GET /api/v1/tasks/{id}`.
    pub(super) task_id: String,
}

/// Read the settings tree at a dot-path.
///
/// The dot-path is the resource identifier. Secrets are redacted in the
/// response. An empty path returns the whole tree. Answers **404** when the
/// path names nothing and **422** when it is not a well-formed dot-path.
#[utoipa::path(
    get,
    path = V1_SETTINGS_DOC,
    context_path = API,
    tag = "resources",
    params(("path" = String, Path, description = "The settings dot-path, verbatim: `hostname`, `access.ssh`, `wifi.ap`")),
    responses(
        (status = 200, description = "The value at the dot-path, redacted", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "The dot-path does not exist (`settings_not_found`)", body = ApiError),
        (status = 422, description = "micad rejected the dot-path (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_settings(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Response {
    resource_response(state.api.get_settings(&path).await, &path)
}

/// What the value at a writable dot-path has to be.
///
/// Four shapes and not one validator per path, because the write surface
/// admits its paths on one ground: each value's validity *"depends on nothing
/// else in the tree"*. A hostname, which `valid_hostname` decides on its own;
/// four switches, which accept either boolean value; and the two `time`
/// values, whose rules ([`micad_settings::validate_timezone_name`] and
/// [`micad_settings::validate_ntp_servers`]) are each self-contained — stated
/// once, in the crate that owns the model, and only CALLED here, so this
/// surface cannot drift from what `Settings::set` enforces. Anything
/// relational is handled by resource routes or micad's reconcilers. In
/// particular, a Wi-Fi client switch does not promise association or resolve
/// a station/AP interface conflict; clients must inspect the apply state.
#[derive(Clone, Copy)]
pub(super) enum ScalarShape {
    /// A JSON string [`valid_hostname`] accepts.
    Hostname,
    /// A JSON boolean, and nothing else.
    Flag,
    /// A JSON string [`micad_settings::validate_timezone_name`] accepts.
    Timezone,
    /// A JSON array of strings [`micad_settings::validate_ntp_servers`]
    /// accepts, written whole — the dot-path syntax has no array indexing.
    NtpServers,
}

/// The dot-paths `PUT /api/v1/settings/{path}` writes, and the shape each
/// value has to have.
///
/// An allowlist rather than a passthrough, and the reason is measured rather
/// than stylistic. `Settings::set`'s documented contract is *"Missing
/// intermediate map entries are created (e.g. setting `network.eth1.dhcp`
/// creates `eth1`)"* (`Settings::set` in `micad-settings/src/model.rs`), so a
/// `PUT` to a mistyped path handed straight through to micad does not fail —
/// it grows a new subtree, of whatever kind the schema defaults to, and the
/// reconciler is the first thing to notice. That exact failure is reachable on
/// `POST /network/peers/add`, where adding a peer to an
/// undeclared `wg9` writes a physical-kind `network.wg9` carrying a WireGuard
/// block. Every path this list does not carry is refused by
/// [`settings_write_refusal`] before any bus call is made.
pub(super) const WRITABLE_SETTINGS: [(&str, ScalarShape); 7] = [
    ("hostname", ScalarShape::Hostname),
    ("access.ssh.enabled", ScalarShape::Flag),
    ("container.enabled", ScalarShape::Flag),
    ("mqtt.enabled", ScalarShape::Flag),
    ("wifi.client.enabled", ScalarShape::Flag),
    ("time.ntp.servers", ScalarShape::NtpServers),
    ("time.timezone", ScalarShape::Timezone),
];

/// The resource the write route's not-found envelope names, being the settings
/// root itself: what is absent is a place in the tree, not an item in a list.
pub(super) const SETTINGS_COLLECTION: &str = "settings";

/// The sentence a refusal carries when nothing more specific is true of the
/// path: it is a real part of the tree, and this route is not how it is
/// written.
pub(super) const WRITABLE_SETTINGS_NOTICE: &str = "this route writes `hostname`, `access.ssh.enabled`, `container.enabled`, `mqtt.enabled`, `wifi.client.enabled`, `time.ntp.servers` and `time.timezone` and no other dot-path; every other subtree is written through its own resource route";

/// The shape `path` is written with, when this route writes it at all.
pub(super) fn writable_shape(path: &str) -> Option<ScalarShape> {
    WRITABLE_SETTINGS
        .iter()
        .find(|(writable, _)| *writable == path)
        .map(|(_, shape)| *shape)
}

/// `value` checked against `shape`, or the sentence the 422 carries.
pub(super) fn check_scalar(shape: ScalarShape, value: &Value) -> Result<(), String> {
    match shape {
        ScalarShape::Flag => value.is_boolean().then_some(()).ok_or_else(|| {
            "this setting is a switch: the body is the JSON literal `true` or `false`".to_string()
        }),
        ScalarShape::Hostname => {
            let name = value
                .as_str()
                .ok_or_else(|| "this setting is text: the body is a JSON string".to_string())?;
            // Not trimmed, unlike `hostname_submit`. That handler trims because
            // a browser sends whatever was typed into a text input; a client
            // that built a JSON string chose its bytes, and silently writing
            // something other than what it sent is the worse answer.
            valid_hostname(name)
                .then_some(())
                .ok_or_else(|| HOSTNAME_RULES.to_string())
        }
        ScalarShape::Timezone => {
            let zone = value
                .as_str()
                .ok_or_else(|| "this setting is text: the body is a JSON string".to_string())?;
            micad_settings::validate_timezone_name(zone)
        }
        ScalarShape::NtpServers => {
            let servers: Vec<String> = value
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| {
                    "this setting is a list: the body is a JSON array of server strings".to_string()
                })?;
            micad_settings::validate_ntp_servers(&servers)
        }
    }
}

/// Whether `root` is a top-level key of the settings schema.
///
/// Derived from `Settings::default()` rather than listed here: every field of
/// that struct serialises unconditionally, so the default tree's key set *is*
/// the schema's top level, and a key added to the struct is covered with no
/// edit in apid. A hand-written list would be a second opinion about a schema
/// that already exists, and second opinions drift.
pub(super) fn is_settings_root(root: &str) -> bool {
    serde_json::to_value(micad_settings::Settings::default())
        .is_ok_and(|tree| tree.get(root).is_some())
}

/// Why this route will not write `path`, in the error envelope.
///
/// Three answers, and they are not interchangeable. **422** is a path that is
/// not a path — an empty segment, an unterminated quote — which is the
/// *malformed* half of the
/// rule. **404** is a path that names nothing, which is the *well-formed but
/// absent* half, and it comes from [`item_not_found`] so the rule is inherited
/// rather than remembered. **409** is a path that names something real which
/// this route does not write, which is the condition 409 already means: the body is well formed and nothing about it is wrong, and what refuses
/// it is the state of the surface.
///
/// "Names nothing" is decided on the **first segment** and not on the whole
/// path, and that is a statement about writes rather than a shortcut. A write's
/// job may be to create the leaf it names — `Settings::set` creates missing
/// intermediates — so a missing leaf is not an absent resource. The one thing
/// a write cannot create is a top-level key the typed schema has no field for:
/// such a tree does not deserialize, so `network.eth9.dhcp` is a write that
/// may legitimately create `eth9`, while `netwrok.eth0.dhcp` can never be
/// anything but a typo.
pub(super) fn settings_write_refusal(path: &str) -> Response {
    let refused = |message: String| {
        api_response(
            StatusCode::CONFLICT,
            ApiError::apid("settings_read_only", message).at(path),
        )
    };
    // `.` is the whole tree, not a malformed path: `Settings::set` documents
    // `""` and `"."` as replacing the root. It is a real path this route
    // refuses, so it takes the refusal rather than the 422 below.
    if path == "." {
        return refused(WRITABLE_SETTINGS_NOTICE.to_string());
    }
    let Some(segments) = micad_settings::path_segments(path) else {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "not a settings dot-path: segments are separated by `.`, a segment is either bare or double-quoted, and no segment is empty".to_string(),
            )
            .at(path),
        );
    };
    // `path_segments` yields at least one segment for every path it accepts.
    let root = segments[0].as_str();
    if !is_settings_root(root) {
        return item_not_found(SETTINGS_COLLECTION, path);
    }
    // `schema_version` is not a settings root: the version lives on each
    // document, not on the tree, so a path naming it names nothing and
    // `is_settings_root` above answers it with the 404 every absent root gets.
    refused(match root {
        // Named rather than folded into the sentence below, because this is
        // the subtree where a passthrough is actively destructive rather than
        // merely wrong.
        "network" => "the `network` subtree is not written through this route: it is written through the typed network routes — `PUT /api/v1/network/{iface}` and the `DELETE` beside it, `PUT /api/v1/network` for the whole map, and the peer collection under each interface. A raw write here would create an entry of the default kind for an interface that has none, and would run none of the relational rules: a bridge naming a port that does not exist would be accepted".to_string(),
        _ => WRITABLE_SETTINGS_NOTICE.to_string(),
    })
}

/// The body of a settings `PUT`: the value to write, and nothing around it.
///
/// A bare JSON value, the same shape `GET` answers.
///
/// The schema is wide because the dot-path decides what is acceptable. What
/// is actually accepted is narrow: a JSON string for `hostname` and
/// `time.timezone`, `true` or `false` for the four switches, and a JSON
/// array of server strings for `time.ntp.servers`. Anything else is **422**.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(transparent)]
pub(crate) struct SettingsWrite(pub(super) Value);

// The success body names the queued apply only; it never echoes the setting,
// which would invite a client to trust the echo over its own GET.
/// Write one scalar setting by dot-path.
///
/// Accepts seven paths and no others: `hostname`, `access.ssh.enabled`,
/// `container.enabled`, `mqtt.enabled`, `wifi.client.enabled`, `time.ntp.servers` and
/// `time.timezone`. Any other path is refused.
///
/// Answers **202** with the queued task id on success. Takes a bearer token.
#[utoipa::path(
    put,
    path = V1_SETTINGS_DOC,
    context_path = API,
    tag = "resources",
    params(("path" = String, Path, description = "The settings dot-path to write: `hostname`, `access.ssh.enabled`, `container.enabled`, `mqtt.enabled`, `wifi.client.enabled`, `time.ntp.servers` or `time.timezone`")),
    request_body = SettingsWrite,
    responses(
        (status = 202, description = "The value was persisted and its scoped reconciliation was queued", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The dot-path names no root the settings schema has (`settings_not_found`)", body = ApiError),
        (status = 409, description = "A dot-path that exists and that this route does not write (`settings_read_only`)", body = ApiError),
        (status = 422, description = "The body carries the redaction sentinel, or is the wrong shape for this setting, or the dot-path is malformed (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_settings_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(path): Path<String>,
    Source(source): Source,
    body: Result<Json<SettingsWrite>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(SettingsWrite(value)) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()).at(&path),
            );
        }
    };
    // Before the allowlist and not after it, deliberately. This is a rule about
    // the body rather than about the path, so a later change that widens the
    // allowlist inherits it instead of stepping around it, and the client this
    // protects — one that read a subtree, edited a field and wrote the whole
    // thing back — is answered about the thing it actually got wrong.
    if redact::carries_sentinel(&value) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "the body carries `{}`, which is what a read substitutes for a secret and never a value to write: writing it back would destroy the credential it stands for. Send only the fields you meant to change",
                    redact::REDACTED
                ),
            )
            .at(&path),
        );
    }
    let Some(shape) = writable_shape(&path) else {
        return settings_write_refusal(&path);
    };
    if let Err(message) = check_scalar(shape, &value) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(&path),
        );
    }
    let task_id = match state.api.set_settings(&path, &value).await {
        Ok(task_id) => task_id,
        Err(err) => return bus_api_error(&err, Some(&path)),
    };
    state.audit.record("settings-write", "accepted", &source);
    // No `access_cache` invalidation, and that is not an omission: micad emits
    // `SettingsChanged` for the path it wrote and the subscription drops the
    // cache for anything under `access`, which is exactly what the `access.ssh`
    // form path already relies on. The token routes invalidate by hand because
    // a revocation must bite on the very next request; nothing here is a
    // credential.
    api_response(StatusCode::ACCEPTED, TaskAccepted { task_id })
}

/// List micad's bounded apply-task history, oldest first.
#[utoipa::path(
    get,
    path = V1_TASKS_PATH,
    context_path = API,
    tag = "tasks",
    responses(
        (status = 200, description = "The bounded apply-task history, oldest first", body = Vec<TaskRecord>),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad returned an invalid task record or failed to answer (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_tasks_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.task_records().await {
        Ok(tasks) => api_response(StatusCode::OK, tasks),
        Err(err) => bus_api_error(&err, None),
    }
}

/// Read one apply task by the id returned with a settings or transient-password write.
#[utoipa::path(
    get,
    path = V1_TASK_ROUTE,
    context_path = API,
    tag = "tasks",
    params(("id" = String, Path, description = "The task id returned by a 202 response")),
    responses(
        (status = 200, description = "The latest known task record", body = TaskRecord),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "No retained task has this id (`task_not_found`)", body = ApiError),
        (status = 500, description = "micad returned an invalid task record or failed to answer (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_task(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.task_record(&id).await {
        Ok(task) => api_response(StatusCode::OK, task),
        Err(err) => task_api_error(&err, &id),
    }
}

/// Read the live-state tree at a dot-path.
///
/// Live state is observed, not configured, and is a separate root from
/// settings. Read-only: there is no write counterpart. Answers **404** when
/// the path names nothing and **422** when it is not a well-formed dot-path.
#[utoipa::path(
    get,
    path = V1_STATE_DOC,
    context_path = API,
    tag = "resources",
    params(("path" = String, Path, description = "The live-state dot-path, verbatim: `hostname`, `network`, `power`")),
    responses(
        (status = 200, description = "The value at the dot-path, redacted", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "The dot-path does not resolve (`settings_not_found`)", body = ApiError),
        (status = 422, description = "micad rejected the dot-path (`settings_rejected`); a dot-path that does not resolve is the 404 above", body = ApiError),
        (status = 500, description = "micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_state(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Response {
    let value = state.api.get_state(&path).await;
    resource_response(value, &path)
}
