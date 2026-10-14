//! Authenticated signed deployment actions through micad.
//! Every mutation is POST-only and uses the shared credential, CSRF and audit gates.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use micad_settings::{REQUESTED, UPDATE_CHECK_EVENT, UPDATE_FETCH_EVENT};

use crate::audit::Source;
use crate::routes::{API, ApiCredential, ApiError, AppState, api_response, bus_api_error};

mod config;
mod deployment;
pub(crate) use config::*;
pub(crate) use deployment::*;

/// The cluster's paths, one spelling each (single-segment, so axum and
/// OpenAPI agree without a doc variant).
pub(crate) const V1_UPDATE_PATH: &str = "/v1/update";
pub(crate) const V1_UPDATE_CHECK_PATH: &str = "/v1/update/check";
pub(crate) const V1_UPDATE_FETCH_PATH: &str = "/v1/update/fetch";
pub(crate) const V1_UPDATE_INSTALL_PATH: &str = "/v1/update/install";
pub(crate) const V1_UPDATE_CONFIRM_PATH: &str = "/v1/update/confirm";
pub(crate) const V1_UPDATE_REJECT_PATH: &str = "/v1/update/reject";
pub(crate) const V1_UPDATE_ROLLBACK_PATH: &str = "/v1/update/rollback";
pub(crate) const V1_UPDATE_REBOOT_OVERRIDE_PATH: &str = "/v1/update/reboot-override";
pub(crate) const V1_UPDATE_CONFIG_PATH: &str = "/v1/update/config";
pub(crate) const V1_UPDATE_IMPORT_PATH: &str = "/v1/update/import";

/// The most an uploaded archive may be.
///
/// A deployment is kernel plus root plus support images; two gigabytes is
/// well above any product this repository builds and well below what a DATA
/// partition holds. The real bound is the device's free space, which the
/// write hits on its own.
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The first eight bytes of every component archive.
const ARCHIVE_MAGIC: &[u8; 8] = b"MICAUPD1";

/// The D-Bus error name micad's update surface refuses policy-forbidden
/// actions with. Not in `routes.rs`'s table: only this cluster produces it.
const FDO_ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// The update state document, verbatim from micad: `lifecycle` (state machine,
/// policy, reboot gate), per-slot status, `booted_slot`, `primary`,
/// `pending_not_confirmed`, `rollback`, `install` and `last_mark`.
///
/// `rollback` is the one place slot state is offered as a decision: micad's
/// `rollback_eligibility` resolves the alternate slot, says whether a manual
/// rollback is permitted, and names the refusal otherwise. There is no second
/// read route for it — this document is the single writer of that fact.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct UpdateState(Value);

/// Map an update-surface bus failure, reading the one name this cluster adds
/// before handing the rest to the shared mapping.
///
/// - `AccessDenied` → **409** `policy_refused`: the request was well-formed
///   and authenticated, and the device's update policy (offline mode,
///   maintenance window, unreadable policy file) or safe-to-reboot gate
///   refused it. The message names the rule.
/// - `InvalidArgs` → **422** `validation_failed`: a mark state, slot or
///   override TTL outside the offered vocabulary. The shared mapping calls
///   this `settings_rejected`, which on a route that writes no setting would
///   misname the fault.
fn update_bus_error(err: &anyhow::Error) -> Response {
    if let Some(zbus::Error::MethodError(name, message, _)) = err.downcast_ref::<zbus::Error>() {
        let message = message.clone().unwrap_or_else(|| name.to_string());
        if name.as_str() == FDO_ACCESS_DENIED {
            return api_response(
                StatusCode::CONFLICT,
                ApiError::micad("policy_refused", message),
            );
        }
        if name.as_str() == "org.freedesktop.DBus.Error.InvalidArgs" {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::micad("validation_failed", message),
            );
        }
    }
    bus_api_error(err, None)
}

/// The envelope for a body that is not JSON or not the declared shape.
fn body_rejection(rejection: axum::extract::rejection::JsonRejection) -> Response {
    api_response(
        StatusCode::BAD_REQUEST,
        ApiError::apid("request_invalid", rejection.body_text()),
    )
}

/// Read the complete update state.
///
/// Reads native deployment records and the current acquisition lifecycle from
/// micad. The same response binds the rollback verdict to its retained target
/// and includes check, download and installation progress.
#[utoipa::path(
    get,
    path = V1_UPDATE_PATH,
    context_path = API,
    tag = "update",
    responses(
        (status = 200, description = "Authenticated native deployment state: `boot` (running deployment and kernel/root IDs, content verification and Secure Boot), `state` (current, fallback, candidate, failed deployment IDs and highestGeneration), `deployments` (version, generation, components and remaining trials), `rollback` (permitted, target and reason), `install`, `last_action`, and `lifecycle` (acquisition progress, workspace, policy and reboot gate). A failure carries a closed code beside its detail: `unknown`, `client-spawn-failed`, `client-exit-failure`, `client-output-unparseable`, `unverified-deployment-path`, `no-source-configured`, `policy-not-loaded`, `probe-failed`, `policy-invalid`, `network-offline`, `network-metered`, `client-unavailable`, `deployment-discarded`, `check-refused`, `no-newer-release`, `fetch-refused`, `clock-untrusted`, `outside-window`, `deployment-status-unknown`, `reboot-pending`, `workspace-unready`, `recheck-failed`, `recheck-refused`, `superseded`, `install-refused`, `reboot-gate-closed`, `install-in-flight`, `health-blocking`. Rollback reasons: `candidate_pending`, `running_not_confirmed`, `no_usable_fallback`. Failed deployment IDs and the generation floor are enforced by the native backend for every install.", body = UpdateState),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad could not read native deployment state (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_state(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_update_state().await {
        Ok(value) => api_response(StatusCode::OK, UpdateState(value)),
        Err(err) => update_bus_error(&err),
    }
}

/// Start an update metadata check.
///
/// Answers **202**: micad admits the check and runs it on a background task;
/// the outcome lands in the state document's `lifecycle` entry.
#[utoipa::path(
    post,
    path = V1_UPDATE_CHECK_PATH,
    context_path = API,
    tag = "update",
    responses(
        (status = 202, description = "The check was admitted and is running; poll `GET /api/v1/update`"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The update policy refused it: offline mode, no configured source, or an unreadable policy file (`policy_refused`)", body = ApiError),
        (status = 500, description = "The update client is absent or another operation is running (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_check(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    match state.api.check_update().await {
        Ok(()) => {
            state.audit.record(UPDATE_CHECK_EVENT, REQUESTED, &source);
            accepted()
        }
        Err(err) => update_bus_error(&err),
    }
}

/// Start a bundle download into the reserve directory.
///
/// Answers **202** like the check; on success the lifecycle records the
/// verified bundle path and enters `ready`.
#[utoipa::path(
    post,
    path = V1_UPDATE_FETCH_PATH,
    context_path = API,
    tag = "update",
    responses(
        (status = 202, description = "The fetch was admitted and is running; poll `GET /api/v1/update`"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The update policy refused it: offline mode, a metered link without `meteredAllowsFetch`, no configured source, or an unreadable policy file (`policy_refused`)", body = ApiError),
        (status = 500, description = "The update client is absent or another operation is running (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_fetch(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    match state.api.fetch_update().await {
        Ok(()) => {
            state.audit.record(UPDATE_FETCH_EVENT, REQUESTED, &source);
            accepted()
        }
        Err(err) => update_bus_error(&err),
    }
}

/// Upload a signed offline archive and import it.
///
/// The body is the `MICAUPD1` archive itself, streamed to the device's upload
/// directory and then imported by micad through `mica-deploy import` -- the
/// same code path an online fetch stages through, so every signature, product
/// and object check an online acquisition runs, runs here.
///
/// **The network policy does not gate it.** A metered link, an absent source
/// and an update mode of `off` all refuse a fetch and none of them has
/// anything to say about a file an operator carried here themselves.
///
/// Answers **202**: the import verifies each object and that is not a
/// request's worth of time. Poll `GET /api/v1/update` for the outcome.
#[utoipa::path(
    post,
    path = V1_UPDATE_IMPORT_PATH,
    context_path = API,
    tag = "actions",
    request_body(content = String, description = "The `MICAUPD1` archive", content_type = "application/octet-stream"),
    responses(
        (status = 202, description = "The archive was written and the import started; poll the update state"),
        (status = 400, description = "The upload was interrupted, or does not begin with the archive magic (`archive_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Another update operation is already running (`update_refused`)", body = ApiError),
        (status = 413, description = "The archive is larger than this device accepts (`archive_too_large`)", body = ApiError),
        (status = 415, description = "The body is not `application/octet-stream` (`archive_type`)", body = ApiError),
        (status = 507, description = "The upload could not be written (`update_storage_unavailable`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_import(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    request: axum::extract::Request<axum::body::Body>,
) -> Response {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let refuse = |status: StatusCode, code: &'static str, message: &str| {
        api_response(status, ApiError::apid(code, message.to_string()))
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
            "archive_type",
            "upload the .micaupd archive as application/octet-stream",
        );
    }
    if request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "archive_too_large",
            "this device accepts an update archive of at most 2 GiB",
        );
    }

    let uploads = state.update_uploads.as_path();
    if let Err(err) = tokio::fs::create_dir_all(uploads).await {
        tracing::error!(error = %err, "creating the update upload directory failed");
        return refuse(
            StatusCode::INSUFFICIENT_STORAGE,
            "update_storage_unavailable",
            "the update upload directory could not be created",
        );
    }
    // A name drawn here and not taken from the client: an upload is addressed
    // by the path apid hands micad, and a client-chosen name is a client
    // choosing where on the device its bytes land.
    let upload = uploads.join(format!(
        "{:032x}.micaupd",
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
            tracing::error!(error = %err, "creating the update upload file failed");
            return refuse(
                StatusCode::INSUFFICIENT_STORAGE,
                "update_storage_unavailable",
                "the upload file could not be created",
            );
        }
    };
    let mut stream = request.into_body().into_data_stream();
    let mut written = 0_u64;
    let mut magic = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                let _ = tokio::fs::remove_file(&upload).await;
                tracing::warn!(error = %err, "the update upload was interrupted");
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "archive_invalid",
                    "the upload was interrupted",
                );
            }
        };
        written += chunk.len() as u64;
        if written > MAX_ARCHIVE_BYTES {
            let _ = tokio::fs::remove_file(&upload).await;
            return refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                "archive_too_large",
                "this device accepts an update archive of at most 2 GiB",
            );
        }
        // The magic is checked as soon as there is enough of it, so a body
        // that is not an archive at all is refused after eight bytes rather
        // than after a gigabyte.
        if magic.len() < ARCHIVE_MAGIC.len() {
            magic.extend_from_slice(&chunk[..chunk.len().min(ARCHIVE_MAGIC.len() - magic.len())]);
            if magic.len() == ARCHIVE_MAGIC.len() && magic != ARCHIVE_MAGIC {
                let _ = tokio::fs::remove_file(&upload).await;
                return refuse(
                    StatusCode::BAD_REQUEST,
                    "archive_invalid",
                    "this is not a component archive: it does not begin with MICAUPD1",
                );
            }
        }
        if let Err(err) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&upload).await;
            tracing::error!(error = %err, "writing the update upload failed");
            return refuse(
                StatusCode::INSUFFICIENT_STORAGE,
                "update_storage_unavailable",
                "the upload could not be written",
            );
        }
    }
    if magic.len() < ARCHIVE_MAGIC.len() {
        let _ = tokio::fs::remove_file(&upload).await;
        return refuse(
            StatusCode::BAD_REQUEST,
            "archive_invalid",
            "this is not a component archive: it does not begin with MICAUPD1",
        );
    }
    if let Err(err) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&upload).await;
        tracing::error!(error = %err, "syncing the update upload failed");
        return refuse(
            StatusCode::INSUFFICIENT_STORAGE,
            "update_storage_unavailable",
            "the upload could not be written",
        );
    }
    drop(file);

    match state.api.import_update(&upload.to_string_lossy()).await {
        Ok(()) => {
            state.audit.record(UPDATE_FETCH_EVENT, REQUESTED, &source);
            accepted()
        }
        Err(err) => {
            // micad never took it, so the copy on disk is apid's to remove.
            let _ = tokio::fs::remove_file(&upload).await;
            update_bus_error(&err)
        }
    }
}

/// The empty 202 the three long-running admissions answer: accepted, running
/// on micad's background task, outcome via the state document.
fn accepted() -> Response {
    use axum::http::header::CACHE_CONTROL;
    use axum::response::IntoResponse;
    (
        StatusCode::ACCEPTED,
        [(
            CACHE_CONTROL,
            crate::assets::mime::CacheClass::NoStore.header_value(),
        )],
    )
        .into_response()
}
