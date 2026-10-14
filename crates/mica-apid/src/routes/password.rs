//! The administrator password and the transient root password.

use crate::access_cache::ACCESS_PATH;
use crate::assets::mime::CacheClass;
use crate::audit::Source;
use crate::auth::{self};
use crate::session::{self};
use axum::Json;
use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use micad_settings::ClaimSettings;
use serde_json::Value;

use super::*;

/// Why one password-change attempt failed, before either surface words it.
///
/// One outcome set for both surfaces: the HTML pane and the API route differ
/// in how they answer, not in what can happen.
pub(super) enum PasswordChangeError {
    /// The current password did not verify; nothing was written.
    WrongCurrent,
    /// The new password is under the same floor the setup wizard enforces;
    /// nothing was written.
    TooShort,
    /// Hashing the new password failed.
    Hashing(anyhow::Error),
    /// A micad call failed.
    Bus(anyhow::Error),
    /// The claim record could not be encoded; the answer is ready.
    Encoding(Box<Response>),
}

/// Verify the current admin password, write the new hash through the settings
/// tree, and drop every session except the acting one.
///
/// The current password is demanded even though the caller holds a session: a
/// session is a browser artifact that outlives the moment the password was
/// typed, and an unattended browser must not be enough to rotate the sole
/// credential on the management surface.
///
/// The invalidation and the write belong in one step. The gate's
/// short-circuit comment says an unset-password operation "has to clear the
/// session table in the same step", and replacing the hash is the same
/// reasoning: a session minted under the old credential proves possession of
/// nothing any more. The acting session is the one exception — it just proved
/// possession of the current password — or the operator would be signed out
/// by their own success.
pub(super) async fn change_password(
    state: &AppState,
    source: &str,
    acting_session: Option<&str>,
    current: &str,
    new: &str,
) -> Result<(), PasswordChangeError> {
    if password_under_floor(new) {
        return Err(PasswordChangeError::TooShort);
    }
    let access = match state.api.get_settings("access").await {
        Ok(value) => value,
        Err(err) => return Err(PasswordChangeError::Bus(err)),
    };
    let Some(hash) = password_hash(&access) else {
        // Unreachable through either surface: both sit behind a verified
        // session, and no session can coexist with an unset password (see
        // `gate`). Refusing is still better than writing a first hash from a
        // route whose contract is rotation.
        return Err(PasswordChangeError::Bus(anyhow::anyhow!(
            "no admin password is configured"
        )));
    };
    // Off the async workers for the same reason login verification is:
    // argon2id costs real CPU per call, by design. A panic in the closure
    // surfaces as a failed verification: closed, never open.
    let hash = hash.to_string();
    let password = current.to_string();
    let verified = tokio::task::spawn_blocking(move || auth::verify_password(&hash, &password))
        .await
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "password verification task failed");
            false
        });
    if !verified {
        state.audit.record("password", "wrong-password", source);
        return Err(PasswordChangeError::WrongCurrent);
    }
    let password = new.to_string();
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .unwrap_or_else(|err| Err(anyhow::anyhow!("password hashing task: {err}")))
        .map_err(PasswordChangeError::Hashing)?;
    // Whether this change is the ROTATION that discharges a bootstrap claim,
    // decided from the tree as it was read above and not from the tree after
    // the write.
    let claim = claim_status(state, &access).await;
    let discharged = claim.rotation_required;
    // The new hash and the record that the bootstrap secret is gone commit
    // together, in ONE write of the whole `access` subtree and therefore one
    // `Store::save`. Two writes could crash between them, and both halves of
    // that crash are wrong: a device still demanding a rotation it has already
    // had, or -- with the writes the other way round -- one excused from a
    // rotation that never landed.
    let mut subtree = match access {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    subtree.insert(
        "webAdmin".to_string(),
        serde_json::json!({ "password_hash": hash }),
    );
    if let Some(via) = claim.via {
        subtree.insert(
            "claim".to_string(),
            encode(ClaimSettings {
                via,
                // WHEN the device was claimed does not move because its
                // credential did. 0 is the reading `ApiToken::created` gives an
                // unset clock, and it is what a derived record carries when the
                // import that claimed the device recorded none.
                at: claim.at.unwrap_or(0),
                rotation_required: false,
            })
            .map_err(PasswordChangeError::Encoding)?,
        );
    }
    if let Err(err) = state
        .api
        .set_settings(ACCESS_PATH, &Value::Object(subtree))
        .await
    {
        return Err(PasswordChangeError::Bus(err));
    }
    // apid knows its own access write happened, so the gate's cache is
    // dropped here rather than waiting for the SettingsChanged round trip:
    // the next unauthenticated request re-reads and cannot be answered from
    // a pre-change snapshot.
    state.access_cache.invalidate();
    // The write happened; every other session goes with the old credential.
    // No cookie on the request keeps nothing, which errs closed.
    state
        .sessions
        .remove_all_except(acting_session.unwrap_or(""));
    state.audit.record("password", "changed", source);
    if discharged {
        // A second line and not a replacement: the password change happened
        // and is recorded as one, and this says what it additionally did to
        // the device's claim. An operator reading the trail for "when did this
        // device stop holding the credential it shipped with" greps one name.
        state
            .audit
            .record(CLAIM_ROTATION_EVENT, "completed", source);
    }
    Ok(())
}

/// `POST /api/v1/actions/change-password` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangePasswordRequest {
    /// The password being replaced, verified before anything is written.
    pub(super) current_password: String,
    /// The replacement; at least 8 characters.
    pub(super) new_password: String,
}

/// Change the admin password.
///
/// Requires the current password. Answers **204** on success; every session
/// but the acting one is dropped. A wrong current password is **401**, and a
/// new password below the length floor is **422**.
#[utoipa::path(
    post,
    path = V1_CHANGE_PASSWORD_PATH,
    context_path = API,
    tag = "actions",
    request_body = ChangePasswordRequest,
    responses(
        (status = 204, description = "The password was changed; every session except the calling one was dropped"),
        (status = 400, description = "The body is not JSON, or not this shape (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The browser CSRF token is invalid (`csrf_invalid`), or the current password does not verify (`wrong_password`)", body = ApiError),
        (status = 422, description = "The new password is shorter than 8 characters (`validation_failed`)", body = ApiError),
        (status = 500, description = "Hashing failed (`hashing_failed`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_change_password(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    headers: HeaderMap,
    body: Result<Json<ChangePasswordRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        // The envelope rather than axum's plain-text rejection.
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()),
            );
        }
    };
    let acting = session::cookie_from_headers(&headers);
    match change_password(
        &state,
        &source,
        acting.as_deref(),
        &request.current_password,
        &request.new_password,
    )
    .await
    {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
        )
            .into_response(),
        Err(PasswordChangeError::WrongCurrent) => api_response(
            StatusCode::FORBIDDEN,
            ApiError::apid(
                "wrong_password",
                "the current password does not verify".to_string(),
            ),
        ),
        Err(PasswordChangeError::TooShort) => api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the new password must be at least 8 characters".to_string(),
            ),
        ),
        Err(PasswordChangeError::Hashing(err)) => {
            tracing::error!(error = %err, "password hashing failed");
            api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid("hashing_failed", format!("{err:#}")),
            )
        }
        Err(PasswordChangeError::Bus(err)) => bus_api_error(&err, Some("access.webAdmin")),
        Err(PasswordChangeError::Encoding(response)) => *response,
    }
}

// Network pane

/// `POST /api/v1/actions/transient-root-password` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransientRootPasswordRequest {
    /// The password to open the channel with: 8 to 72 bytes, and no NUL,
    /// newline or carriage return.
    pub(super) password: String,
}

// Bounds are checked by the same function the form path calls, not a second
// copy, and none of its three messages interpolates the password.
/// Set a transient root password.
///
/// The password must be 8 to 72 bytes and contain no NUL, newline or carriage
/// return. 72 is bcrypt's limit — a longer password would be silently
/// truncated, so it is refused instead.
///
/// The password is written into no setting, is never logged, and lasts until
/// the next reboot. A **422** states which bound was broken and never repeats
/// the password back.
///
/// Answers **202** once the password hash is written and its scoped apply is queued.
#[utoipa::path(
    post,
    path = V1_TRANSIENT_PASSWORD_PATH,
    context_path = API,
    tag = "actions",
    request_body = TransientRootPasswordRequest,
    responses(
        (status = 202, description = "The transient root password hash was written and its scoped apply was queued", body = TaskAccepted),
        (status = 400, description = "The body is not JSON, or not this shape (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The password is shorter than 8 bytes, longer than 72, or contains a NUL, newline or carriage return (`validation_failed`); the message states the bound and never the password", body = ApiError),
        (status = 500, description = "micad failed to set it (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_transient_root_password(
    _credential: ApiCredential,
    State(app): State<AppState>,
    Source(source): Source,
    body: Result<Json<TransientRootPasswordRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        // The envelope rather than axum's plain-text rejection. The
        // rejection text describes the shape, never the value, so a malformed
        // body carrying a password does not put it in the response either.
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()),
            );
        }
    };
    if let Err(message) = validate_transient_password(&request.password) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message),
        );
    }
    let task_id = match app.api.set_transient_root_password(&request.password).await {
        Ok(task_id) => task_id,
        Err(err) => {
            // No dot-path: this writes no setting, so the optional member is
            // absent rather than naming something that was not at fault.
            return bus_api_error(&err, None);
        }
    };
    // The event carries who opened a password channel and from where — and
    // deliberately nothing about the password itself.
    app.audit.record("transient-password", "set", &source);
    api_response(StatusCode::ACCEPTED, TaskAccepted { task_id })
}
