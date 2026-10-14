//! Browser sessions: status, login and logout.

use crate::assets::mime::CacheClass;
use crate::audit::Source;
use crate::auth::{self};
use crate::session::{self};
use axum::Json;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::*;

/// The browser's current authentication state.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionStatus {
    /// `setup`, `unauthenticated`, or `authenticated`.
    pub(super) state: &'static str,
    /// Returned only for an authenticated browser session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) csrf_token: Option<String>,
}

impl SessionStatus {
    pub(super) fn setup() -> Self {
        Self {
            state: "setup",
            csrf_token: None,
        }
    }

    pub(super) fn unauthenticated() -> Self {
        Self {
            state: "unauthenticated",
            csrf_token: None,
        }
    }

    pub(super) fn authenticated(csrf_token: String) -> Self {
        Self {
            state: "authenticated",
            csrf_token: Some(csrf_token),
        }
    }
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct SessionLoginRequest {
    pub(super) password: String,
}

/// Report setup and browser authentication state without redirecting.
#[utoipa::path(
    get,
    path = V1_SESSION_PATH,
    context_path = API,
    tag = "session",
    responses(
        (status = 200, description = "Setup and browser authentication state", body = SessionStatus),
        (status = 500, description = "The access settings could not be read", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_session_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let access = match access_settings(&state).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some("access")),
    };
    if password_hash(&access).is_none() {
        return api_response(StatusCode::OK, SessionStatus::setup());
    }
    let status = session::cookie_from_headers(&headers)
        .and_then(|cookie| state.sessions.csrf_token(&cookie))
        .map_or_else(SessionStatus::unauthenticated, SessionStatus::authenticated);
    api_response(StatusCode::OK, status)
}

/// Authenticate a browser and create its HttpOnly session cookie.
#[utoipa::path(
    post,
    path = V1_SESSION_PATH,
    context_path = API,
    tag = "session",
    request_body = SessionLoginRequest,
    responses(
        (status = 201, description = "The browser session was created", body = SessionStatus),
        (status = 400, description = "The body is not JSON", body = ApiError),
        (status = 401, description = "The password is incorrect", body = ApiError),
        (status = 409, description = "The device still requires first-run setup", body = ApiError),
        (status = 422, description = "The JSON body has the wrong shape", body = ApiError),
        (status = 429, description = "Login attempts are temporarily throttled", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_session_create(
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: SessionLoginRequest = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if !state.guard.begin_attempt() {
        state.audit.record("login", "throttled", &source);
        return api_response(
            StatusCode::TOO_MANY_REQUESTS,
            ApiError::apid(
                "login_throttled",
                "too many failed logins; retry shortly".to_string(),
            ),
        );
    }
    let generation = state.sessions.generation();
    let access = match state.api.get_settings("access").await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some("access")),
    };
    let Some(hash) = password_hash(&access) else {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "setup_required",
                "complete first-run setup before signing in".to_string(),
            ),
        );
    };
    let hash = hash.to_string();
    let password = request.password;
    let verified = tokio::task::spawn_blocking(move || auth::verify_password(&hash, &password))
        .await
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "password verification task failed");
            false
        });
    if !verified {
        state.guard.confirm_failure();
        state.audit.record("login", "wrong-password", &source);
        return api_response(
            StatusCode::UNAUTHORIZED,
            ApiError::apid(
                "invalid_credentials",
                "the password is incorrect".to_string(),
            ),
        );
    }

    let Some(session) = state.sessions.create_if_current(generation) else {
        state.audit.record("login", "credential-changed", &source);
        return api_response(
            StatusCode::UNAUTHORIZED,
            ApiError::apid(
                "invalid_credentials",
                "the credential changed during sign-in; sign in again".to_string(),
            ),
        );
    };
    state.guard.record_success();
    state.audit.record("login", "success", &source);
    (
        StatusCode::CREATED,
        [
            (
                CACHE_CONTROL,
                CacheClass::NoStore.header_value().to_string(),
            ),
            (SET_COOKIE, state.session_cookie(&session.cookie)),
        ],
        Json(SessionStatus::authenticated(session.csrf_token)),
    )
        .into_response()
}

/// Revoke the acting browser session.
#[utoipa::path(
    delete,
    path = V1_SESSION_PATH,
    context_path = API,
    tag = "session",
    responses(
        (status = 204, description = "The browser session was revoked"),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 403, description = "The browser CSRF token is absent or invalid", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_session_delete(
    State(state): State<AppState>,
    Source(source): Source,
    credential: ApiCredential,
) -> Response {
    if let ApiCredential::Session(cookie) = credential {
        state.sessions.remove(&cookie);
        state.audit.record("logout", "ok", &source);
    }
    (
        StatusCode::NO_CONTENT,
        [
            (
                CACHE_CONTROL,
                CacheClass::NoStore.header_value().to_string(),
            ),
            (SET_COOKIE, state.clear_cookie()),
        ],
    )
        .into_response()
}
