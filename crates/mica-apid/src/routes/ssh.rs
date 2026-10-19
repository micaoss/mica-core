//! SSH authorized keys.

use crate::assets::mime::CacheClass;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use micad_settings::{
    AuthorizedKey, SettingsError, parse_authorized_key, validate_authorized_keys,
};
use serde_json::Value;

use super::*;

/// One row of `GET /api/v1/ssh/authorized-keys`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct AuthorizedKeyEntry {
    /// Canonical `<type> <blob>` key text, with the comment in its own field
    /// -- what the parser stored, and not what was submitted.
    pub(super) key: String,
    /// The operator's label, absent when the key was stored without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) comment: Option<String>,
    /// This entry's `DELETE` path segment: `SHA256:` followed by the unpadded
    /// base64 of the SHA-256 digest of the decoded blob — the string
    /// `ssh-keygen -lf` prints.
    ///
    /// `null` for a stored line whose blob does not decode. Nothing this route
    /// writes can be in that state -- `parse_authorized_key` refuses it -- but
    /// the settings file is an editable file on STATE, so a list read back is
    /// not necessarily a list this API wrote.
    pub(super) fingerprint: Option<String>,
}

/// `GET /api/v1/ssh/authorized-keys` response body.
///
/// An object and not a bare array, because of `notice`: the sentence is a
/// property of the collection rather than of any entry, and the contract
/// requires it on the listing **and** on the add. A client that only ever adds keys must still be told what a key
/// grants.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct AuthorizedKeyList {
    /// Every stored key, in stored order.
    pub(super) keys: Vec<AuthorizedKeyEntry>,
    /// Why a key added here is not a key with limited access.
    pub(super) notice: String,
}

/// `POST /api/v1/ssh/authorized-keys` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct AddAuthorizedKeyRequest {
    /// One authorized-key line, `<type> <blob>` with an optional trailing
    /// comment: exactly what the pane's field takes, handed to exactly the
    /// same parser.
    pub(super) key: String,
}

/// `POST /api/v1/ssh/authorized-keys` response body.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct AddedAuthorizedKey {
    /// The entry as it was stored, canonicalised by the parser, carrying the
    /// fingerprint that is now its `DELETE` path segment.
    pub(super) key: AuthorizedKeyEntry,
    /// The same sentence the listing carries.
    pub(super) notice: String,
}

/// `SHA256:`, the prefix every fingerprint this device computes carries.
pub(super) const FINGERPRINT_PREFIX: &str = "SHA256:";

/// Characters of unpadded base64 a 32-byte digest occupies.
///
/// A SHA-256 digest is 32 bytes, and unpadded base64 spends four characters on
/// every three bytes: 43 for 32 bytes, with no padding written.
pub(super) const FINGERPRINT_DIGEST_CHARS: usize = 43;

/// Whether `value` is spelled like a fingerprint at all.
///
/// This is what gives the status rule both of its halves on the SSH item
/// route. A string that could never be a fingerprint is **422**, because it
/// names nothing and could name nothing; a well-formed fingerprint that
/// matches no stored key is **404**, from [`item_not_found`].
///
/// The alphabet includes `/` and `+`: it is standard base64 and not the
/// URL-safe variant, because that is what `ssh-keygen -lf` prints and what
/// [`ssh_fingerprint`] computes. A fingerprint carrying a `/` reaches this
/// route percent-encoded; see [`collection_item`].
pub(super) fn is_ssh_fingerprint(value: &str) -> bool {
    value
        .strip_prefix(FINGERPRINT_PREFIX)
        .is_some_and(|digest| {
            digest.len() == FINGERPRINT_DIGEST_CHARS
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
        })
}

/// A stored key as the API answers it.
pub(super) fn key_entry(entry: AuthorizedKey) -> AuthorizedKeyEntry {
    AuthorizedKeyEntry {
        fingerprint: ssh_fingerprint(&entry.key),
        key: entry.key,
        comment: entry.comment,
    }
}

/// The stored authorized-key list, or the error envelope for whatever prevented
/// reading it.
///
/// The API's own read and not [`stored_keys`], for the reason the token
/// collection has two as well: the pane's helper flattens a failed bus call and
/// an unreadable stored list into one `anyhow::Error`, and those are answered
/// with different statuses. An unreadable list is an error and never an
/// empty list -- treating it as empty would let an add overwrite keys the
/// operator cannot see.
pub(super) async fn api_stored_keys(state: &AppState) -> Result<Vec<AuthorizedKey>, Box<Response>> {
    let ssh = match state.api.get_settings("access.ssh").await {
        Ok(value) => value,
        Err(err) => return Err(Box::new(bus_api_error(&err, Some(SSH_KEYS_PATH)))),
    };
    parse_key_list(&ssh).map_err(|err| {
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid("settings_invalid", err.to_string()).at(SSH_KEYS_PATH),
        ))
    })
}

/// Validate and write a rewritten key list, in the error envelope.
///
/// The API's own writer and not [`write_key_list`], which answers a re-rendered
/// pane at 422 and a redirect on success. The **validator** is the same one:
/// `validate_authorized_keys` is what micad's SSH reconciler runs before it
/// renders the file, so a list either surface accepts is a list the reconciler
/// accepts too.
pub(super) async fn api_write_keys(
    state: &AppState,
    keys: &[AuthorizedKey],
) -> Result<(), Box<Response>> {
    if let Err(err) = validate_authorized_keys(keys) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", key_error_message(&err)).at(SSH_KEYS_PATH),
        )));
    }
    // Infallible: `AuthorizedKey` is a struct of strings with no map keys that
    // could collide.
    let value = encode(keys)?;
    if let Err(err) = state.api.set_settings(SSH_KEYS_PATH, &value).await {
        return Err(Box::new(bus_api_error(&err, Some(SSH_KEYS_PATH))));
    }
    Ok(())
}

/// List every authorized SSH key.
///
/// Each entry carries the fingerprint that identifies it for removal. The
/// response also carries a `notice`: every authorized key grants root.
#[utoipa::path(
    get,
    path = V1_SSH_KEYS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The stored keys, each with the fingerprint that is its `DELETE` path segment, and the notice every client of this collection is told", body = AuthorizedKeyList),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a key list (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ssh_keys_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match api_stored_keys(&state).await {
        Ok(keys) => api_response(
            StatusCode::OK,
            AuthorizedKeyList {
                keys: keys.into_iter().map(key_entry).collect(),
                notice: ROOT_KEY_NOTICE.to_string(),
            },
        ),
        Err(response) => *response,
    }
}

// The line is parsed exactly as submitted, untrimmed: surrounding whitespace
// is one of the things the parser exists to reject, and trimming here would
// accept a line micad would not.
/// Authorize one SSH public key.
///
/// The key line is validated as submitted; a malformed line is **422**.
///
/// **Every authorized key grants root.** `AuthorizedKeysFile` is `%u`-expanded
/// over one shared list, so the response carries a `notice` saying so
/// alongside the created entry.
#[utoipa::path(
    post,
    path = V1_SSH_KEYS_PATH,
    context_path = API,
    tag = "resources",
    request_body = AddAuthorizedKeyRequest,
    responses(
        (status = 201, description = "The key was authorized; the body carries it canonicalised, with its fingerprint and the notice", body = AddedAuthorizedKey),
        (status = 400, description = "The body is not JSON, or not this shape (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "A stored key already carries that public key (`key_exists`), or the device already holds the maximum number of keys (`key_limit_reached`); the collection's current state is what refuses the request, not the body", body = ApiError),
        (status = 422, description = "The line is not an authorized key, or the resulting list is one the SSH reconciler would refuse (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a key list (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ssh_keys_add(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<AddAuthorizedKeyRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return api_response(
                StatusCode::BAD_REQUEST,
                ApiError::apid("request_invalid", rejection.body_text()).at(SSH_KEYS_PATH),
            );
        }
    };
    let parsed = match parse_authorized_key(&request.key) {
        Ok(parsed) => parsed,
        Err(err) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", key_error_message(&err)).at(SSH_KEYS_PATH),
            );
        }
    };
    let mut keys = match api_stored_keys(&state).await {
        Ok(keys) => keys,
        Err(response) => return *response,
    };
    // Both refusals below are 409 and both are decided **here**, before the
    // shared validator runs, and neither reads a validator message to find out
    // what happened. `validate_authorized_keys` refuses a duplicate and a full
    // list too -- it has to, because the settings file is writable without apid
    // -- but it refuses them as one `Validation` error alongside a malformed
    // key, and inferring which by matching its words would be a parser for
    // prose. The comparison and the bound are both available here, so the
    // answer is decided from the collection rather than recovered from a
    // sentence.
    if keys.iter().any(|stored| stored.key == parsed.key) {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "key_exists",
                "a stored key already carries that public key; remove it before adding it again, and change its label with a remove and an add".to_string(),
            )
            .at(SSH_KEYS_PATH),
        );
    }
    // The cap, answered from `micad_settings::MAX_KEYS` exactly as the token
    // mint answers its own from `MAX_TOKENS`.
    if keys.len() >= micad_settings::MAX_KEYS {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "key_limit_reached",
                format!(
                    "this device already holds the maximum of {} authorized keys; remove one first",
                    micad_settings::MAX_KEYS
                ),
            )
            .at(SSH_KEYS_PATH),
        );
    }
    keys.push(parsed.clone());
    if let Err(response) = api_write_keys(&state, &keys).await {
        return *response;
    }
    api_response(
        StatusCode::CREATED,
        AddedAuthorizedKey {
            key: key_entry(parsed),
            notice: ROOT_KEY_NOTICE.to_string(),
        },
    )
}

// 404 here where the HTML pane answers 422 on the same condition, and that
// split is deliberate: a path segment has one interpretation, a typed form
// field does not. Paired tests name each other so it cannot read as drift.
/// Remove one authorized SSH key by its fingerprint.
///
/// The fingerprint is the only accepted identifier; the key text is not.
/// Answers **404** when no authorized key carries it.
#[utoipa::path(
    delete,
    path = V1_SSH_KEY_ROUTE,
    context_path = API,
    tag = "resources",
    params(("fingerprint" = String, Path, description = "The key's fingerprint, as `GET /api/v1/ssh/authorized-keys` returns it: `SHA256:` and 43 base64 characters. Its alphabet contains `/`, so a fingerprint carrying one is percent-encoded")),
    responses(
        (status = 204, description = "The key was removed; the reconciler has re-rendered the authorized-keys file without it"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No stored key has that fingerprint (`settings_not_found`). Well-formed and absent, which is a different answer from malformed", body = ApiError),
        (status = 422, description = "The path segment is not a fingerprint at all (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored list could not be read as a key list (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ssh_keys_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
) -> Response {
    if !is_ssh_fingerprint(&fingerprint) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "an authorized-key fingerprint is `{FINGERPRINT_PREFIX}` followed by {FINGERPRINT_DIGEST_CHARS} base64 characters, as `ssh-keygen -lf` prints it"
                ),
            )
            .at(SSH_KEYS_PATH),
        );
    }
    let mut keys = match api_stored_keys(&state).await {
        Ok(keys) => keys,
        Err(response) => return *response,
    };
    let found = keys
        .iter()
        .position(|entry| ssh_fingerprint(&entry.key).as_deref() == Some(fingerprint.as_str()));
    let Some(index) = found else {
        return item_not_found(SSH_KEYS_PATH, &fingerprint);
    };
    keys.remove(index);
    if let Err(response) = api_write_keys(&state, &keys).await {
        return *response;
    }
    (
        StatusCode::NO_CONTENT,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
    )
        .into_response()
}

/// Settings dot-path of the stored authorized-key list.
pub(super) const SSH_KEYS_PATH: &str = "access.ssh.authorizedKeys";

/// The sentence the pane has to carry, verbatim.
///
/// `AuthorizedKeysFile` is `%u`-expanded over one shared key list, so a key
/// added here logs in as root. An operator who adds a colleague's key expecting
/// an unprivileged shell would be handing out root, and a pane that says
/// nothing manufactures exactly that misunderstanding. A test asserts the
/// sentence renders, so a later refactor cannot quietly drop it.
pub(super) const ROOT_KEY_NOTICE: &str = "Every authorized key is a root key.";

/// Shortest transient password accepted, in bytes; micad's own floor.
pub(super) const MIN_TRANSIENT_PASSWORD_BYTES: usize = 8;

/// Longest transient password accepted, in bytes.
///
/// The 72 is not arbitrary, and it is deliberately tighter than micad's own
/// bound: the transient password is hashed with bcrypt, and bcrypt reads only
/// the first 72 bytes of its input and silently ignores the rest. Accepting a
/// 100-character password would therefore mean the first 72 characters of it
/// also unlock the device — the operator would be running on a shorter secret
/// than the one they typed and believe in. Refusing the input is the only way
/// the pane avoids creating that surprise; truncating it silently would be the
/// same surprise with a different author.
pub(super) const MAX_TRANSIENT_PASSWORD_BYTES: usize = 72;

// Recomputed rather than read from the published state: the pane maps the
// fingerprint an operator clicks back onto the stored entry a removal rewrites,
// and the published list carries no such handle.
pub(super) use micad_settings::ssh_fingerprint;

/// The parser's own message, without the dot-path prefix its `Display` adds:
/// the operator is looking at a form field, not at a settings path.
pub(super) fn key_error_message(err: &SettingsError) -> String {
    match err {
        SettingsError::Validation { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Read the stored key list out of the `access.ssh` subtree.
///
/// An absent list is an empty list, but a list that is present and unreadable
/// is an error rather than an empty list: treating it as empty
/// would let an add or a remove overwrite keys the operator cannot see.
pub(super) fn parse_key_list(ssh: &Value) -> anyhow::Result<Vec<AuthorizedKey>> {
    match ssh.get("authorizedKeys") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|err| anyhow::anyhow!("The stored authorized-key list is unreadable: {err}")),
    }
}

/// Bounds and forbidden bytes for a transient password.
///
/// No message echoes the password, and no branch here logs it: the only place
/// it goes is the D-Bus call.
pub(super) fn validate_transient_password(password: &str) -> Result<(), String> {
    if password.len() < MIN_TRANSIENT_PASSWORD_BYTES {
        return Err(format!(
            "Password must be at least {MIN_TRANSIENT_PASSWORD_BYTES} bytes."
        ));
    }
    if password.len() > MAX_TRANSIENT_PASSWORD_BYTES {
        return Err(format!(
            "Password must be at most {MAX_TRANSIENT_PASSWORD_BYTES} bytes: it is hashed with bcrypt, which reads only the first {MAX_TRANSIENT_PASSWORD_BYTES} bytes, so a longer one would be silently shortened to that."
        ));
    }
    if password.contains(['\0', '\n', '\r']) {
        return Err("Password must not contain a NUL, newline or carriage return.".to_string());
    }
    Ok(())
}
