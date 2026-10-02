//! The reboot override and the update configuration.

use crate::audit::Source;
use crate::routes::{API, ApiCredential, ApiError, AppState, api_response};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::UPDATE_CONFIG_EVENT;
use serde_json::Value;

use super::*;

/// `POST /api/v1/update/reboot-override` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RebootOverrideRequest {
    /// Override TTL in seconds: at least 1, at most the policy's
    /// `overrideMaxSeconds` (never above 3600).
    pub(super) seconds: u32,
}

/// The armed override, as micad recorded it.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RebootOverride(pub(super) Value);

/// Arm the bounded administrative override of the safe-to-reboot gate.
///
/// Lifts health-report blocks for the TTL — never an install in flight —
/// and expires on its own. Audited on both sides: this route records the
/// event, and micad logs who armed it and until when.
#[utoipa::path(
    post,
    path = V1_UPDATE_REBOOT_OVERRIDE_PATH,
    context_path = API,
    tag = "update",
    request_body = RebootOverrideRequest,
    responses(
        (status = 200, description = "The override is armed until the answered instant", body = RebootOverride),
        (status = 400, description = "The body is not JSON or not this shape (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "A TTL of zero or above the granted ceiling (`validation_failed`)", body = ApiError),
        (status = 500, description = "micad failed to arm it (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_reboot_override(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<RebootOverrideRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    match state.api.set_reboot_override(request.seconds).await {
        Ok(record) => {
            state
                .audit
                .record("update-reboot-override", "armed", &source);
            api_response(StatusCode::OK, RebootOverride(record))
        }
        Err(err) => update_bus_error(&err),
    }
}

/// The operator's update document as micad saved it: the layer-2 keys and
/// nothing resolved.
///
/// A key the operator never set is **absent** and one they set to `null` is
/// **`null`**; both mean "take the baked default" and they are still two
/// different statements about the document. The resolved values — what this
/// device will actually dial and follow — are `GET /api/v1/update`'s
/// `lifecycle.policy` and `GET /api/v1/provisioning/status`'s `effective`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct UpdateConfig(pub(super) Value);

/// The body of the update-configuration write: a **patch**, not a document.
///
/// A key this body omits is left exactly as it is on the device; a key set to
/// `null` clears an operator override so the baked default applies again; a
/// key with a value overrides it. That is what makes the write safe from a
/// console: a client that sent the whole resolved document back would pin the
/// image's defaults into the operator's layer and the device would stop
/// following its image, which nobody asked for.
///
/// Accepted keys: `policy` (`off`/`check`/`auto`), `checkIntervalMinutes`,
/// `checkAt` (`HH:MM` UTC, the time of day the check is anchored to),
/// `rebootPolicy` (`manual`/`window`), `source.url`, `network`,
/// `maintenance` and `rebootGate`. Anything else — including a
/// trust anchor under any name, at any depth — is **422** naming the key: the
/// address this device dials is the operator's, what it will accept is baked
/// into the image, and no body may move that line.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(transparent)]
pub(crate) struct UpdateConfigWrite(pub(super) Value);

/// Write the operator's update configuration.
///
/// The one route that changes what this device does on its own: which address
/// it dials, whether it checks, fetches and installs
/// unattended, and inside which maintenance window. It takes the same
/// administrator authority every other management write takes, and there is no
/// unauthenticated or fleet-derived path to it.
///
/// **apid does not write the file.** micad owns `/mica/config/updates.json` and
/// is its only writer; this route asks. The validation is therefore the same
/// code the update subsystem reads the document with, so a document that is
/// accepted here is one that loads, and a rejected one is refused with the
/// offending field named **before** anything is replaced. `auto` with no
/// maintenance window is refused here rather than failing closed some hours
/// later at a check the operator would have to go looking for.
///
/// Answers **200** with the document as saved.
#[utoipa::path(
    post,
    path = V1_UPDATE_CONFIG_PATH,
    context_path = API,
    tag = "update",
    request_body = UpdateConfigWrite,
    responses(
        (status = 200, description = "The operator document as saved: the keys layer 2 carries, absent and `null` still distinct", body = UpdateConfig),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The document already on the device does not load, so there is no base to change; nothing was written (`policy_refused`)", body = ApiError),
        (status = 422, description = "The patch names a key this document does not have, a trust anchor, or a value the reader would refuse — `auto` with no maintenance window, a window that is not `HH:MM` (`validation_failed`)", body = ApiError),
        (status = 500, description = "micad could not store it (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_config(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<UpdateConfigWrite>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(UpdateConfigWrite(patch)) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    match state.api.set_update_config(&patch).await {
        Ok(document) => {
            state.audit.record(UPDATE_CONFIG_EVENT, "written", &source);
            api_response(StatusCode::OK, UpdateConfig(document))
        }
        Err(err) => update_bus_error(&err),
    }
}
