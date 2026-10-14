//! Reboot and power-off.

use crate::assets::mime::CacheClass;
use crate::audit::Source;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};

use super::*;

/// A power action the pane can request of micad.
#[derive(Clone, Copy)]
pub(super) enum PowerAction {
    Reboot,
    PowerOff,
}

impl PowerAction {
    /// Stable action name used in the audit trail.
    pub(super) fn confirm_token(self) -> &'static str {
        match self {
            Self::Reboot => "reboot",
            Self::PowerOff => "poweroff",
        }
    }
}

/// Return acceptance only after micad admits the power request.
///
/// The bounded bus call reports policy refusals and dispatch failures. Its
/// success means shutdown was requested, not that the machine has restarted.
/// Audit before dispatch because the connection may disappear during shutdown.
pub(super) async fn power_accepted(
    state: &AppState,
    action: PowerAction,
    source: &str,
) -> Response {
    state
        .audit
        .record(action.confirm_token(), "requested", source);
    let result = match action {
        PowerAction::Reboot => state.api.reboot().await,
        PowerAction::PowerOff => state.api.power_off().await,
    };
    if let Err(err) = result {
        state.audit.record(action.confirm_token(), "failed", source);
        if let Some(zbus::Error::MethodError(name, message, _)) = err.downcast_ref::<zbus::Error>()
            && name.as_str() == "org.freedesktop.DBus.Error.AccessDenied"
        {
            tracing::warn!(action = action.confirm_token(), error = %err, "power action refused");
            return api_response(
                StatusCode::CONFLICT,
                ApiError::micad(
                    "power_refused",
                    message.clone().unwrap_or_else(|| name.to_string()),
                ),
            );
        }
        return bus_api_error(&err, None);
    }
    (
        StatusCode::ACCEPTED,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
    )
        .into_response()
}

/// Reboot the appliance.
///
/// Answers **202**: the request is accepted and the reboot is dispatched, so
/// there may be no connection left to carry a later status.
#[utoipa::path(
    post,
    path = V1_REBOOT_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 202, description = "micad admitted the reboot request; completion is not reported over this connection"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "micad refused the power action (`power_refused`); the message states the policy or permission reason", body = ApiError),
        (status = 500, description = "micad failed to dispatch the power action (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "The bounded micad call timed out; the outcome is not confirmed (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_reboot(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    power_accepted(&state, PowerAction::Reboot, &source).await
}

/// Power the appliance off.
///
/// Answers **202**: the request is accepted and the power-off is dispatched,
/// so there may be no connection left to carry a later status.
#[utoipa::path(
    post,
    path = V1_POWEROFF_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 202, description = "micad admitted the power-off request; completion is not reported over this connection"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "micad refused the power action (`power_refused`); the message states the policy or permission reason", body = ApiError),
        (status = 500, description = "micad failed to dispatch the power action (`micad_failed`)", body = ApiError),
        (status = 503, description = "micad is unreachable (`micad_unreachable`)", body = ApiError),
        (status = 504, description = "The bounded micad call timed out; the outcome is not confirmed (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_poweroff(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    power_accepted(&state, PowerAction::PowerOff, &source).await
}

// Hostname submit

// SSH pane
