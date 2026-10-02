//! The API credential a request carries: session or bearer token.

use crate::audit::Source;
use crate::session::{self};
use crate::token;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use serde_json::Value;

use super::*;

/// Authentication accepted by management API handlers.
///
/// Automation uses a stored bearer token. The built-in and custom SPAs use a
/// signed browser session; state-changing requests made with that session must
/// also present its `X-CSRF-Token` value. Keeping the check in the extractor
/// makes it impossible for a newly added authenticated mutation to forget the
/// browser-side protection.
///
/// The forced rotation that bounds a bootstrap claim is enforced here for the
/// same reason and by the same argument: it applies to every authenticated
/// mutation the API serves, so it belongs in the one place every authenticated
/// mutation already passes through, rather than in a list of routes that a
/// later one can be added outside of.
pub(crate) enum ApiCredential {
    Bearer,
    Session(String),
}

impl FromRequestParts<AppState> for ApiCredential {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let mutation = matches!(
            parts.method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        );
        let credential = if bearer_is_stored(state, &parts.headers).await {
            Self::Bearer
        } else if let Some(cookie) = session::cookie_from_headers(&parts.headers)
            && state.sessions.verify(&cookie)
        {
            if mutation {
                let presented = parts
                    .headers
                    .get("x-csrf-token")
                    .and_then(|value| value.to_str().ok());
                if !presented.is_some_and(|token| state.sessions.verify_csrf(&cookie, token)) {
                    return Err(api_response(
                        StatusCode::FORBIDDEN,
                        ApiError::apid(
                            "csrf_invalid",
                            "a browser session mutation requires its X-CSRF-Token value"
                                .to_string(),
                        ),
                    ));
                }
            }
            Self::Session(cookie)
        } else {
            return Err(not_authenticated(
                "this route requires a stored bearer token or an authenticated browser session",
            ));
        };
        if mutation && bound_by_rotation(parts.uri.path()) && rotation_required(state).await {
            // Reads stay open, and so do the two mutations that write nothing
            // to the DEVICE. Those exemptions are what make the bound
            // impossible to brick a device with: the operator holding the
            // bootstrap credential can always sign in and out, can always see
            // WHY they were refused (`GET /api/v1/claim`), and can always do
            // the one thing that clears it. Everything else waits.
            let Source(source) = Source::from_request_parts(parts, state)
                .await
                .expect("the source extractor is infallible");
            state.audit.record(CLAIM_ROTATION_EVENT, "refused", &source);
            return Err(api_response(
                StatusCode::CONFLICT,
                ApiError::apid(
                    "rotation_required",
                    "this device was claimed with a bootstrap credential that has not been \
                     rotated; change the administrator password with \
                     `POST /api/v1/actions/change-password` before any other write"
                        .to_string(),
                )
                .at("access.claim"),
            ));
        }
        Ok(credential)
    }
}

/// The 401, in whichever wording the rejecting extractor owes.
pub(super) fn not_authenticated(message: &str) -> Response {
    api_response(
        StatusCode::UNAUTHORIZED,
        ApiError::apid("not_authenticated", message.to_string()),
    )
}

/// Whether the request carries a bearer token this device stores.
///
/// **Not rate limited, and it must not become so.** The secret is 256 bits of
/// the system CSPRNG and is not guessable online, while a shared counter here would let
/// anyone holding a bad token lock out every script on the appliance. The login
/// backoff ([`auth::GuardStore`]) stays scoped to the password path, which is
/// where a human-chosen secret is.
pub(super) async fn bearer_is_stored(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(presented) = token::bearer_from_headers(headers) else {
        return false;
    };
    // The subtree the gate already reads, which is why the list lives under
    // `access` rather than beside it: no second round trip per request.
    let access = match access_settings(state).await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "reading `access` for a bearer check failed");
            return false;
        }
    };
    // A stored list that does not parse authenticates nobody, which errs
    // closed -- the opposite of the read the token routes do, where the same
    // condition is an error rather than an empty list because a write follows.
    token::verify(&parse_tokens(&access).unwrap_or_default(), presented)
}

/// The `access` subtree: from the gate's cache when it is provably fresh, and
/// from micad otherwise.
///
/// The generation is snapshotted BEFORE the direct read so a change signalled
/// while the read was in flight discards the fill rather than caching a
/// possibly-pre-change snapshot.
pub(super) async fn access_settings(state: &AppState) -> anyhow::Result<Value> {
    if let Some(value) = state.access_cache.get() {
        return Ok(value);
    }
    let generation = state.access_cache.generation();
    let value = state.api.get_settings("access").await?;
    state.access_cache.fill(generation, value.clone());
    Ok(value)
}

/// Whether `access` (the settings subtree) carries an admin password hash.
pub(super) fn password_hash(access: &Value) -> Option<&str> {
    access
        .get("webAdmin")
        .and_then(|admin| admin.get("password_hash"))
        .and_then(Value::as_str)
}
