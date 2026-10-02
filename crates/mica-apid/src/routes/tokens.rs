//! API tokens.

use crate::assets::mime::CacheClass;
use crate::token;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use micad_settings::{ApiToken, validate_api_tokens};
use serde_json::Value;

use super::*;

// Three members and not four: the digest is not on this wire and neither is
// the plaintext.
/// One row of `GET /api/v1/tokens`.
///
/// Carries no secret: the token's plaintext appears in exactly one response,
/// when it is minted, and never again.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ApiTokenSummary {
    /// The token's stable identity, which is also its `DELETE` path segment.
    /// Not secret: it is a lookup key, and it is on the wire for that.
    pub(super) id: String,
    /// The operator's label, the only thing that tells one token from another.
    pub(super) name: String,
    /// Seconds since the UNIX epoch as the device clock read them at the mint.
    ///
    /// **A label, never a deadline.** No unit on this image syncs a clock, so
    /// the reading may be wrong by any amount and 0 means the clock was unset.
    /// Tokens do not expire; revocation is the whole lifecycle.
    pub(super) created: u64,
}

/// `POST /api/v1/tokens` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct MintTokenRequest {
    /// The label the new token is listed under.
    pub(super) name: String,
}

/// `POST /api/v1/tokens` response body: the one place a plaintext token
/// appears.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct MintedToken {
    /// The new token's identity, for a later `DELETE`.
    pub(super) id: String,
    /// The label as it was submitted.
    pub(super) name: String,
    /// The whole token, `mica_<id>_<secret>`.
    ///
    /// **It appears here and nowhere else, ever.** Only the SHA-256 digest is
    /// stored, so a token that is lost is replaced and never recovered -- the
    /// posture `access.device` already takes.
    pub(super) token: String,
}

/// List every API token this device holds.
///
/// Returns each token's id, label and creation time. Secrets are not stored
/// and are never returned — a token's secret is shown once, when it is
/// minted.
#[utoipa::path(
    get,
    path = V1_TOKENS_PATH,
    context_path = API,
    tag = "tokens",
    responses(
        (status = 200, description = "The stored tokens: `id`, `name` and `created`, never the digest and never the plaintext", body = Vec<ApiTokenSummary>),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a token list (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_tokens_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match stored_tokens(&state).await {
        Ok(tokens) => api_response(
            StatusCode::OK,
            tokens
                .into_iter()
                .map(|entry| ApiTokenSummary {
                    id: entry.id,
                    name: entry.name,
                    created: entry.created,
                })
                .collect::<Vec<_>>(),
        ),
        Err(response) => *response,
    }
}

// Cookie deliberately not accepted: that would put a permanent-credential
// factory on the browser surface.
/// Mint an API token.
///
/// Takes either a stored bearer token or an authenticated browser session. The
/// secret is returned once, in this response, and is not retrievable
/// afterwards.
///
/// A signed-in browser can mint the first additional token through this API.
#[utoipa::path(
    post,
    path = V1_TOKENS_PATH,
    context_path = API,
    tag = "tokens",
    request_body = MintTokenRequest,
    responses(
        (status = 201, description = "The token was created; the body carries the plaintext, which is not recoverable afterwards", body = MintedToken),
        (status = 400, description = "The body is not JSON, or not this shape (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device already holds the maximum number of tokens (`token_limit_reached`); revoke one first", body = ApiError),
        (status = 422, description = "The name is empty, over 256 bytes, or holds a control character (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a token list (`settings_invalid`), no free id was drawn (`mint_failed`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_tokens_mint(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<MintTokenRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()),
            );
        }
    };
    let mut tokens = match stored_tokens(&state).await {
        Ok(tokens) => tokens,
        Err(response) => return *response,
    };
    // The cap is answered here and not only by the store. The validator makes
    // a full list a hard refusal, and without this check the caller meets that
    // refusal as a failed write -- a 500 about micad -- rather than as an answer
    // about the request they made. 409 and not 422: the body is well formed and
    // nothing about it is wrong, and what refuses it is the collection's
    // current state, which is the condition 409 already means.
    if tokens.len() >= micad_settings::MAX_TOKENS {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "token_limit_reached",
                format!(
                    "this device already holds the maximum of {} API tokens; revoke one before minting another",
                    micad_settings::MAX_TOKENS
                ),
            )
            .at(API_TOKENS_PATH),
        );
    }
    let Some(minted) = token::mint(&tokens) else {
        return api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "mint_failed",
                "no free token id was drawn; nothing was written".to_string(),
            )
            .at(API_TOKENS_PATH),
        );
    };
    tokens.push(ApiToken {
        id: minted.id.clone(),
        name: request.name.clone(),
        hash: minted.hash,
        created: device_clock_seconds(),
    });
    if let Err(response) = write_tokens(&state, &tokens).await {
        return *response;
    }
    api_response(
        StatusCode::CREATED,
        MintedToken {
            id: minted.id,
            name: request.name,
            token: minted.wire,
        },
    )
}

// Identity is the id and never a list position: an index is meaningful only
// against the list the caller last read, and a concurrent mint slides it.
/// Revoke one API token by its id.
///
/// Takes effect on the next request; the token set is read per request.
/// Answers **404** when no token carries that id.
#[utoipa::path(
    delete,
    path = V1_TOKEN_ROUTE,
    context_path = API,
    tag = "tokens",
    params(("id" = String, Path, description = "The token id, as `POST /api/v1/tokens` returned it: 1 to 64 lowercase hex characters")),
    responses(
        (status = 204, description = "The token was revoked; it stops being accepted on the next request"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No stored token carries that id (`settings_not_found`). Well-formed and absent, which is a different answer from malformed", body = ApiError),
        (status = 422, description = "The id is not a token id at all (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a token list (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_tokens_revoke(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    // Malformed and absent are different answers and must not share a status.
    // An id that is not an id could never name
    // an entry, so a 404 here would send the caller looking for a token they
    // deleted instead of at the URL they typed.
    if !micad_settings::is_api_token_id(&id) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "a token id is 1 to 64 lowercase hex characters".to_string(),
            )
            .at(API_TOKENS_PATH),
        );
    }
    let mut tokens = match stored_tokens(&state).await {
        Ok(tokens) => tokens,
        Err(response) => return *response,
    };
    let Some(index) = tokens.iter().position(|entry| entry.id == id) else {
        return item_not_found(API_TOKENS_PATH, &id);
    };
    tokens.remove(index);
    if let Err(response) = write_tokens(&state, &tokens).await {
        return *response;
    }
    (
        StatusCode::NO_CONTENT,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
    )
        .into_response()
}

/// The stored token list, or the error envelope for whatever prevented reading it.
///
/// The read is direct rather than from the gate's `access` cache: this is the
/// read half of a read-modify-write, and the freshest list is the one least
/// likely to drop somebody else's entry.
pub(super) async fn stored_tokens(state: &AppState) -> Result<Vec<ApiToken>, Box<Response>> {
    let access = match state.api.get_settings("access").await {
        Ok(value) => value,
        Err(err) => return Err(Box::new(bus_api_error(&err, Some(API_TOKENS_PATH)))),
    };
    parse_tokens(&access).map_err(|err| {
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored token list could not be read: {err}"),
            )
            .at(API_TOKENS_PATH),
        ))
    })
}

/// The token list inside an `access` subtree.
///
/// An absent list is an empty list -- the model omits the field entirely when
/// nothing is stored -- but a list that is present and unreadable is an error
/// and never an empty list, for the reason the SSH key list gives: treating it
/// as empty would let a mint or a revoke overwrite tokens the operator cannot
/// see.
pub(super) fn parse_tokens(access: &Value) -> Result<Vec<ApiToken>, serde_json::Error> {
    match access.get("apiTokens") {
        Some(value) => serde_json::from_value(value.clone()),
        None => Ok(Vec::new()),
    }
}

/// Validate and write a rewritten token list.
///
/// Read-modify-write of the whole array, because the dot-path syntax has no
/// array indexing -- the same pattern the SSH key pane uses. **Two concurrent
/// mints lose one token, silently**: both read the list, both append to their
/// own copy, and the second write wins. It is recorded and not fixed; it is a
/// cost inherited from the tree, and the alternative is a locking
/// scheme this codebase does not have.
pub(super) async fn write_tokens(
    state: &AppState,
    tokens: &[ApiToken],
) -> Result<(), Box<Response>> {
    // The same validator micad runs, so a list this route accepts is one the
    // store will accept too. Its message names an entry index and never echoes
    // a digest or an id.
    if let Err(err) = validate_api_tokens(tokens) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", key_error_message(&err)).at(API_TOKENS_PATH),
        )));
    }
    // Infallible: `ApiToken` is a struct of scalars with no map keys to collide.
    let value = encode(tokens)?;
    if let Err(err) = state.api.set_settings(API_TOKENS_PATH, &value).await {
        return Err(Box::new(bus_api_error(&err, Some(API_TOKENS_PATH))));
    }
    // apid knows its own `access` write happened, so the gate's cache is
    // dropped here rather than waiting for the `SettingsChanged` round trip.
    // The bearer check reads the same subtree, and this is what makes a
    // revocation take effect on the next request.
    state.access_cache.invalidate();
    Ok(())
}

/// The device clock in seconds since the UNIX epoch, saturating at 0.
///
/// A label and never a deadline; see [`ApiTokenSummary::created`]. A clock
/// before the epoch reads 0 rather than failing a mint, because an untrusted
/// clock must not decide whether the operator may hold a credential.
pub(super) fn device_clock_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

// The two array collections: the SSH
// authorized keys, whose identity is a fingerprint, and the WiFi station's
// known networks, whose identity is an SSID.
