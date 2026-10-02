//! Installing, confirming, rejecting and rolling back a deployment.

use crate::audit::Source;
use crate::routes::{API, ApiCredential, ApiError, AppState, api_response};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::{REQUESTED, UPDATE_INSTALL_EVENT};
use serde_json::Value;

use super::*;

/// An exact signed deployment identity already verified in the acquisition workspace.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeploymentRequest {
    #[serde(deserialize_with = "deployment_id")]
    #[schema(min_length = 64, max_length = 64, pattern = "^[0-9a-f]{64}$")]
    pub(super) deployment_id: String,
}

pub(super) fn valid_deployment_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn deployment_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let id = <String as serde::Deserialize>::deserialize(deserializer)?;
    if !valid_deployment_id(&id) {
        return Err(serde::de::Error::custom(
            "deploymentId must be 64 lowercase hexadecimal characters",
        ));
    }
    Ok(id)
}

/// Install an acquired signed deployment; poll update state for completion.
#[utoipa::path(post, path = V1_UPDATE_INSTALL_PATH, context_path = API, tag = "update",
    request_body = DeploymentRequest,
    responses(
        (status = 202, description = "The deployment action was accepted"),
        (status = 400, description = "Invalid JSON or deployment identity (`request_invalid`)", body = ApiError),
        (status = 401, description = "Authentication required (`not_authenticated`)", body = ApiError),
        (status = 403, description = "Browser CSRF token required (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Operator policy refused the action (`policy_refused`)", body = ApiError),
        (status = 422, description = "Deployment identity or acquisition path is invalid (`validation_failed`)", body = ApiError),
        (status = 500, description = "The native backend refused or failed the action (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "POST required (`method_not_allowed`)", body = ApiError),
    ))]
pub(crate) async fn api_v1_update_install(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<DeploymentRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    match state.api.install_update(&request.deployment_id).await {
        Ok(()) => {
            state.audit.record(
                UPDATE_INSTALL_EVENT,
                &format!("{} {}", REQUESTED, request.deployment_id),
                &source,
            );
            api_response(
                StatusCode::ACCEPTED,
                serde_json::json!({"deploymentId":request.deployment_id}),
            )
        }
        Err(error) => update_bus_error(&error),
    }
}

/// Confirm the authenticated running deployment explicitly.
#[utoipa::path(post, path = V1_UPDATE_CONFIRM_PATH, context_path = API, tag = "update",
    request_body = DeploymentRequest,
    responses(
        (status = 200, description = "The deployment action was accepted"),
        (status = 400, description = "Invalid JSON or deployment identity (`request_invalid`)", body = ApiError),
        (status = 401, description = "Authentication required (`not_authenticated`)", body = ApiError),
        (status = 403, description = "Browser CSRF token required (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Operator policy refused the action (`policy_refused`)", body = ApiError),
        (status = 422, description = "Deployment identity or acquisition path is invalid (`validation_failed`)", body = ApiError),
        (status = 500, description = "The native backend refused or failed the action (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "POST required (`method_not_allowed`)", body = ApiError),
    ))]
pub(crate) async fn api_v1_update_confirm(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<DeploymentRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    match state.api.confirm_deployment(&request.deployment_id).await {
        Ok(()) => {
            state.audit.record(
                "update-confirm",
                &format!("{} {}", REQUESTED, request.deployment_id),
                &source,
            );
            api_response(
                StatusCode::OK,
                serde_json::json!({"deploymentId":request.deployment_id}),
            )
        }
        Err(error) => update_bus_error(&error),
    }
}

/// Reject a deployment while retaining a usable fallback.
#[utoipa::path(post, path = V1_UPDATE_REJECT_PATH, context_path = API, tag = "update",
    request_body = DeploymentRequest,
    responses(
        (status = 200, description = "The deployment action was accepted"),
        (status = 400, description = "Invalid JSON or deployment identity (`request_invalid`)", body = ApiError),
        (status = 401, description = "Authentication required (`not_authenticated`)", body = ApiError),
        (status = 403, description = "Browser CSRF token required (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Operator policy refused the action (`policy_refused`)", body = ApiError),
        (status = 422, description = "Deployment identity or acquisition path is invalid (`validation_failed`)", body = ApiError),
        (status = 500, description = "The native backend refused or failed the action (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "POST required (`method_not_allowed`)", body = ApiError),
    ))]
pub(crate) async fn api_v1_update_reject(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<DeploymentRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    match state.api.reject_deployment(&request.deployment_id).await {
        Ok(()) => {
            state.audit.record(
                "update-reject",
                &format!("{} {}", REQUESTED, request.deployment_id),
                &source,
            );
            api_response(
                StatusCode::OK,
                serde_json::json!({"deploymentId":request.deployment_id}),
            )
        }
        Err(error) => update_bus_error(&error),
    }
}

pub(super) const ROLLBACK_REASONS: [&str; 3] = [
    "candidate_pending",
    "running_not_confirmed",
    "no_usable_fallback",
];

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RollbackResponse {
    pub(super) deployment_id: String,
    pub(super) target: String,
    pub(super) next_step: String,
}

pub(super) fn rollback_reason_code(reason: Option<&str>) -> &'static str {
    reason
        .and_then(|reason| ROLLBACK_REASONS.into_iter().find(|known| *known == reason))
        .unwrap_or("rollback_refused")
}

/// Reject the confirmed running deployment and select its retained fallback.
/// The native backend revalidates this operation under its transaction lock.
/// Reboot remains a separate action governed by the shared reboot gate.
#[utoipa::path(post, path = V1_UPDATE_ROLLBACK_PATH, context_path = API, tag = "update",
    responses(
        (status = 200, description = "Rollback committed; reboot to run the retained fallback", body = RollbackResponse),
        (status = 401, description = "Authentication required (`not_authenticated`)", body = ApiError),
        (status = 403, description = "Browser CSRF token required (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Native state refuses rollback: `candidate_pending`, `running_not_confirmed`, `no_usable_fallback`, or `rollback_refused`", body = ApiError),
        (status = 500, description = "The native backend refused or failed rollback (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "POST required (`method_not_allowed`)", body = ApiError),
    ))]
pub(crate) async fn api_v1_update_rollback(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    let update = match state.api.get_update_state().await {
        Ok(update) => update,
        Err(error) => return update_bus_error(&error),
    };
    let reason = update.pointer("/rollback/reason").and_then(Value::as_str);
    let target = update
        .pointer("/rollback/target")
        .and_then(Value::as_str)
        .filter(|id| valid_deployment_id(id));
    let running = update
        .pointer("/boot/deploymentId")
        .and_then(Value::as_str)
        .filter(|id| valid_deployment_id(id));
    let permitted = update
        .pointer("/rollback/permitted")
        .and_then(Value::as_bool)
        == Some(true);
    let (Some(target), Some(running), true) = (target, running, permitted) else {
        let code = rollback_reason_code(reason);
        state
            .audit
            .record("update-rollback", &format!("refused: {code}"), &source);
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                code,
                format!(
                    "the native deployment state does not permit rollback: {}",
                    reason.unwrap_or("missing or invalid verdict")
                ),
            ),
        );
    };
    match state.api.rollback_deployment(running).await {
        Ok(()) => {
            state.audit.record(
                "update-rollback",
                &format!("{running} to {target}"),
                &source,
            );
            api_response(
                StatusCode::OK,
                RollbackResponse {
                    deployment_id: running.into(),
                    target: target.into(),
                    next_step: "POST /api/v1/actions/reboot".into(),
                },
            )
        }
        Err(error) => update_bus_error(&error),
    }
}
