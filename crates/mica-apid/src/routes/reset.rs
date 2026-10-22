//! Factory reset and credential recovery.

use crate::access_cache::ACCESS_PATH;
use crate::audit::Source;
use crate::auth::{self};
use crate::session::{self};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use micad_settings::{ClaimChannel, ClaimSettings, ResetTier};
use serde_json::Value;

use super::*;

/// `POST /api/v1/reset` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct ResetRequest {
    /// Which tier. A tier names itself in the request and in the audit record;
    /// there is no parameterless reset.
    #[schema(value_type = String, example = "configuration")]
    pub(super) tier: ResetTier,
}

/// `POST /api/v1/reset` response body.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetStaged {
    /// The tier that is now staged.
    #[schema(value_type = String, example = "configuration")]
    pub(super) tier: ResetTier,
    /// When it runs. Always `next-boot`: a reset is an intent
    /// record plus an idempotent apply, and micad applies it before anything
    /// else on the next boot.
    pub(super) applies: &'static str,
}

/// `POST /api/v1/recovery/credential` response body.
///
/// **It carries no secret and there is no member it could carry one in.**
/// The credential is returned on the channel that proved presence,
/// never over the network, so the body says that a credential was minted and
/// where it went — the operator standing at the console reads it there.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CredentialRecovered {
    /// The mechanism that proved presence and published the credential — the
    /// board's own name for it, so the operator reading this and the operator
    /// reading the audit trail are looking at one word.
    pub(super) mechanism: String,
    /// `access.device.generation` after the rotation.
    pub(super) generation: u32,
}

/// The refusal, in the error envelope.
pub(super) fn presence_refusal(reason: &NoPresence) -> Response {
    api_response(
        StatusCode::FORBIDDEN,
        ApiError::apid("presence_required", reason.message()),
    )
}

/// Stage a reset tier.
///
/// **This route stages; micad applies.** A reset is an intent
/// record plus an idempotent apply, so the whole of this handler's write is
/// ONE `SetSettings("reset")` — one `Store::save` — after which a power loss
/// leaves the device either not asked or asked, never half-reset. micad carries
/// the tier out before anything else on the next boot and clears the record.
///
/// **Authority.** Both tiers are authenticated management actions.
/// `ResetTier` has two members, so `secure-wipe` is a body this route cannot
/// parse, and `full-factory` is answered with the route that erases the
/// device, `POST /api/v1/system/factory-reset`.
///
/// A tier staged over another replaces it. Both are audited, so nothing is
/// silent, and the alternative — refusing until something un-stages the first
/// — is a dead end for an operator who asked for the wrong one.
#[utoipa::path(
    post,
    path = V1_RESET_PATH,
    context_path = API,
    tag = "actions",
    request_body = ResetRequest,
    responses(
        (status = 202, description = "The tier is staged and runs on the next boot", body = ResetStaged),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 409, description = "The device was claimed with a bootstrap credential that has not been rotated (`rotation_required`)", body = ApiError),
        (status = 422, description = "The body is not this shape, or names no tier this route stages (`validation_failed`); `full-factory` is `POST /api/v1/system/factory-reset`", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_reset(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Ok(Json(value)) = &body
        && value["tier"] == "full-factory"
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!(
                    "`full-factory` is not a reset tier; the factory reset is \
                     `POST {API}{V1_FACTORY_RESET_PATH}`"
                ),
            )
            .at("tier"),
        );
    }
    let request: ResetRequest = match json_body(body, Some(RESET_PATH)) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let event = micad_settings::reset_event(request.tier);

    let intent = serde_json::json!({
        "tier": request.tier,
        "requested": device_clock_seconds(),
    });
    if let Err(err) = state.api.set_settings(RESET_PATH, &intent).await {
        return bus_api_error(&err, Some(RESET_PATH));
    }
    state.audit.record(event, "staged", &source);
    api_response(
        StatusCode::ACCEPTED,
        ResetStaged {
            tier: request.tier,
            applies: "next-boot",
        },
    )
}

/// The audit event of the factory reset.
pub(super) const FACTORY_RESET_EVENT: &str = "factory-reset";

/// `POST /api/v1/system/factory-reset` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FactoryResetRequest {
    /// The administrator password, checked whatever the session or token.
    pub(super) password: String,
}

/// `POST /api/v1/system/factory-reset` response body.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct FactoryResetStaged {
    /// Always `reboot`: the device reboots in a few seconds and erases
    /// before anything else runs.
    pub(super) applies: &'static str,
}

/// Why a factory reset cannot run now.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct FactoryResetRefusal {
    /// `factory_reset_unsupported`, `not_claimed`, `update_in_progress` or
    /// `trial_pending`.
    pub(super) code: String,
    pub(super) message: String,
}

/// The last completed factory reset.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct FactoryResetDone {
    /// Unix seconds, as the device's clock read them.
    pub(super) requested: u64,
    /// Unix seconds, as the device's clock read them early in the boot that
    /// erased.
    pub(super) completed: u64,
    /// `console`, `api` or `cli`.
    pub(super) by: String,
    /// The peer or mechanism that asked.
    pub(super) source: String,
    /// Entries the erase could not remove; 0 on a clean erase.
    pub(super) unremoved: u64,
}

/// `GET /api/v1/system/factory-reset` response body.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct FactoryResetStatus {
    /// Whether this device's boot carries out a factory reset.
    pub(super) supported: bool,
    /// Why it cannot run now, the password aside; null when it can.
    pub(super) refusal: Option<FactoryResetRefusal>,
    /// The last completed reset; null when there was none.
    pub(super) last: Option<FactoryResetDone>,
}

/// Whether a factory reset can run now, and the last one.
#[utoipa::path(
    get,
    path = V1_FACTORY_RESET_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 200, description = "Whether the reset is supported, why it cannot run now, and the last one", body = FactoryResetStatus),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad answered something else (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_factory_reset_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let value = match state.api.get_factory_reset().await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, None),
    };
    match serde_json::from_value::<FactoryResetStatus>(value) {
        Ok(status) => api_response(StatusCode::OK, status),
        Err(err) => api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::micad("micad_failed", format!("factory reset status: {err}")),
        ),
    }
}

/// Erase everything configured on the device: the administrator and the
/// claim, settings, networks, keys, apps, containers and all data. The
/// software stays at its current version. The device reboots a few seconds
/// after the answer and erases before anything else runs; it then starts as
/// on its first boot, unclaimed. It is not a certified wipe: the files are
/// removed and the freed space is discarded where the storage supports it.
///
/// **The administrator password is required**, whatever the session or
/// token. A wrong one counts in the login backoff, the same counter
/// `POST /api/v1/login` keeps.
#[utoipa::path(
    post,
    path = V1_FACTORY_RESET_PATH,
    context_path = API,
    tag = "actions",
    request_body = FactoryResetRequest,
    responses(
        (status = 202, description = "The reset is staged; the device reboots into it in a few seconds", body = FactoryResetStaged),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The reset cannot run now: the device's boot does not carry one out (`factory_reset_unsupported`), the device is not claimed (`not_claimed`), an update is being installed or awaits its reboot (`update_in_progress`), or a deployment or core set is on trial (`trial_pending`)", body = ApiError),
        (status = 422, description = "The body is not this shape, or the password is empty (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 500, description = "micad failed to stage it (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out; the outcome is not confirmed (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_factory_reset(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: FactoryResetRequest = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    if request.password.is_empty() {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the administrator password is required".to_string(),
            )
            .at("password"),
        );
    }
    if !state.guard.begin_attempt() {
        state
            .audit
            .record(FACTORY_RESET_EVENT, "throttled", &source);
        return api_response(
            StatusCode::TOO_MANY_REQUESTS,
            ApiError::apid(
                "login_throttled",
                "too many failed attempts; retry shortly".to_string(),
            ),
        );
    }
    let by = match credential {
        ApiCredential::Session(_) => "console",
        ApiCredential::Bearer | ApiCredential::Local => "api",
    };
    let Err(err) = state
        .api
        .factory_reset(&request.password, by, &source)
        .await
    else {
        state.guard.record_success();
        state.audit.record(FACTORY_RESET_EVENT, "staged", &source);
        return api_response(
            StatusCode::ACCEPTED,
            FactoryResetStaged { applies: "reboot" },
        );
    };
    let Some((code, status, message)) = factory_reset_refusal(&err) else {
        return bus_api_error(&err, None);
    };
    let outcome = match code {
        "password_incorrect" => {
            state.guard.confirm_failure();
            "wrong-password"
        }
        "login_throttled" => "throttled",
        _ => "refused",
    };
    state.audit.record(FACTORY_RESET_EVENT, outcome, &source);
    let error = ApiError::micad(code, message);
    api_response(
        status,
        if code == "password_incorrect" {
            error.at("password")
        } else {
            error
        },
    )
}

/// micad's `FactoryResetRefused`, as the code, the status it answers and
/// the message.
fn factory_reset_refusal(err: &anyhow::Error) -> Option<(&'static str, StatusCode, String)> {
    let (name, description) = err.downcast_ref::<zbus::Error>().and_then(error_reply)?;
    if name != "com.mica.micad1.Error.FactoryResetRefused" {
        return None;
    }
    let refusal: FactoryResetRefusal = serde_json::from_str(&description?).ok()?;
    let (code, status) = match refusal.code.as_str() {
        "password_incorrect" => ("password_incorrect", StatusCode::FORBIDDEN),
        "login_throttled" => ("login_throttled", StatusCode::TOO_MANY_REQUESTS),
        "not_claimed" => ("not_claimed", StatusCode::CONFLICT),
        "factory_reset_unsupported" => ("factory_reset_unsupported", StatusCode::CONFLICT),
        "update_in_progress" => ("update_in_progress", StatusCode::CONFLICT),
        "trial_pending" => ("trial_pending", StatusCode::CONFLICT),
        _ => return None,
    };
    Some((code, status, refusal.message))
}

/// Recover the management credential: rotate, never reveal.
///
/// **Presence is the authority, and the only one.** An authenticated
/// management session may NOT run this flow — a session that can rotate the
/// credential it authenticated with is a session-fixation lever, and an
/// operator holding a working credential needs
/// `POST /api/v1/actions/change-password` rather than recovery. So the route
/// takes no credential extractor and refuses a caller that presents one.
///
/// **What it does, in order.** It MINTS a new password — it never
/// discloses, decrypts or derives the previous secret, and there is no code
/// path from here to a stored plaintext. It publishes the new one exactly
/// once, on the channel that proved presence. Only then does it commit, in ONE
/// write of `access`: the new hash, the emptied token list, the claim record
/// and the bumped generation together. Publishing first is the safe
/// direction — a crash between the two must not be a self-inflicted lockout —
/// so the worst outcome here is a credential the operator saw and that never
/// worked, which the flow being cheap to re-run answers.
///
/// **The previous credential stops working at that commit**, and so does every
/// API token: "any client or automation holding it must
/// be re-enrolled". Every session goes with it, for the reason a password
/// change drops them.
///
/// **A device claimed by a provisioning document.** Its
/// bootstrap secret sat in plaintext on a medium, and losing it before the
/// forced rotation is a recovery case rather than a claim case. A recovery writes the claim record with the channel that claimed the
/// device preserved and `rotationRequired` FALSE: the credential this flow
/// mints was drawn by the device from the system CSPRNG and shown once at the device, so
/// it is not a bootstrap secret, and demanding a rotation of a credential that
/// was just rotated under physical presence would be a bound with nothing left
/// to protect.
///
/// **An unclaimed device is refused**, pointing at `POST /api/v1/setup`. There
/// is nothing to recover on a device that has no credential, and minting one
/// here would be another channel that can claim a device —
/// `micad_settings::ClaimChannel` names every one and says why.
///
/// **One rotation per presence assertion**. The assertion is read and
/// spent inside one guard, so a second request — concurrent, or later inside
/// the same fifteen-minute window — finds it spent and is refused `403`
/// `presence_required`, having minted, published and written nothing. The next
/// rotation needs presence asserted again at the device.
// NOT a doc comment, deliberately: everything above is PUBLISHED — `utoipa`
// copies this rustdoc into `apid/openapi.json` as the operation's description,
// and `the_committed_openapi_document_is_the_generated_one` fails until the
// committed document is regenerated. So what goes above is the contract a
// caller needs; the reasoning behind the guard is in the body, at the guard. A
// note to the next editor does not belong in a published API document either,
// which is what this line is.
#[utoipa::path(
    post,
    path = V1_RECOVERY_CREDENTIAL_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 200, description = "A new credential was minted and published on the channel that proved presence; the body carries no secret", body = CredentialRecovered),
        (status = 403, description = "Physical presence is not asserted, has expired or has already been spent by a rotation (`presence_required`), or the caller is authenticated and must use `POST /api/v1/actions/change-password` instead (`authenticated_session`)", body = ApiError),
        (status = 409, description = "The device has no administrator credential to recover; claim it with `POST /api/v1/setup` (`not_claimed`)", body = ApiError),
        (status = 500, description = "Hashing the new password failed (`hash_failed`), it could not be published on the presence channel (`publish_failed`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_recovery_credential(
    State(state): State<AppState>,
    Source(source): Source,
    headers: HeaderMap,
) -> Response {
    // Bare until presence establishes a mechanism: a refusal has no door to
    // record. Everything after the assertion below is recorded under
    // `credential-recovery-<mechanism>`, which is the board's declared name
    // for the door that was used.
    let event = CREDENTIAL_RECOVERY_EVENT;

    // The third authority does not exist, and the check that it does not is
    // here rather than in a comment. A caller holding a working credential is
    // turned away BEFORE presence is consulted: what they are told is which
    // route they should have used, and that answer does not depend on whether
    // anyone is standing at the device.
    if bearer_is_stored(&state, &headers).await
        || session::cookie_from_headers(&headers)
            .is_some_and(|cookie| state.sessions.verify(&cookie))
    {
        state.audit.record(event, "refused", &source);
        return api_response(
            StatusCode::FORBIDDEN,
            ApiError::apid(
                "authenticated_session",
                "credential recovery is authorized by physical presence and not by a session; \
                 an operator holding a working credential changes it with \
                 `POST /api/v1/actions/change-password`"
                    .to_string(),
            ),
        );
    }

    // The other bound, and the one the code did not keep: "one rotation per
    // presence assertion, and the assertion is re-performed physically for the
    // next one". Held from BEFORE the assertion is read to AFTER it is spent,
    // so the two are one step.
    let _rotation = state.rotation.lock().await;

    // The first rule: a presence-gated rotation is NOT throttled by the
    // login guard. The guard slows a remote guesser, presence is not
    // guessable, and a device whose operator is standing in front of it must
    // not be made to wait out a window an attacker armed. Nothing on this path
    // calls `begin_attempt`, and that absence is the rule.
    let assertion = match state.presence.assert() {
        Ok(assertion) => assertion,
        Err(reason) => {
            state.audit.record(event, "refused", &source);
            return presence_refusal(&reason);
        }
    };
    let event = &micad_settings::credential_recovery_event(&assertion.mechanism);

    let access = match state.api.get_settings(ACCESS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(ACCESS_PATH)),
    };
    if password_hash(&access).is_none() {
        state.audit.record(event, "refused", &source);
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "not_claimed",
                "this device has no administrator credential to recover; claim it with \
                 `POST /api/v1/setup`"
                    .to_string(),
            )
            .at("access.webAdmin"),
        );
    }
    let claim = claim_status(&state, &access).await;

    let secret = mint_recovery_password();
    let hashed = secret.clone();
    let hash = match tokio::task::spawn_blocking(move || auth::hash_password(&hashed))
        .await
        .unwrap_or_else(|err| Err(anyhow::anyhow!("password hashing task: {err}")))
    {
        Ok(hash) => hash,
        Err(err) => {
            // Logged and not returned, for the setup route's reason: the error
            // carries argon2's own text and the caller can do nothing with it.
            tracing::error!(error = %err, "recovery password hashing failed");
            state.audit.record(event, "aborted", &source);
            return api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "hash_failed",
                    "the new credential could not be hashed; nothing was written".to_string(),
                ),
            );
        }
    };

    // Published BEFORE the commit, the safe direction. A failure here
    // is the `aborted`: presence was established and the flow did not
    // complete, the previous credential still works, and nothing was written.
    if let Err(err) = state.presence.publish(&assertion, &secret) {
        tracing::error!(error = %err, "publishing the recovered credential failed");
        state.audit.record(event, "aborted", &source);
        return api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "publish_failed",
                "the new credential could not be published on the channel that proved \
                 presence; nothing was written and the previous credential still works"
                    .to_string(),
            ),
        );
    }

    // The commit: ONE write of the whole `access` subtree, so the new hash,
    // the revoked tokens, the claim record and the bumped generation land
    // together. Two writes could crash between them and leave a device whose
    // old credential was invalidated and whose new one was not stored.
    let generation = device_generation(&access).saturating_add(1);
    let mut subtree = match access {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    subtree.insert(
        "webAdmin".to_string(),
        serde_json::json!({ "password_hash": hash }),
    );
    // Every bearer token is a management credential too, and the previous one
    // stops working with the credential: a recovery that left them
    // authenticating would leave whoever holds one exactly the access the
    // operator came to the device to take back.
    subtree.insert("apiTokens".to_string(), Value::Array(Vec::new()));
    let claim = match encode(ClaimSettings {
        // A device claimed by a document carries no record, and the channel
        // that claimed it is still that document — the rotation moves the
        // credential, not the history.
        via: claim.via.unwrap_or(ClaimChannel::ProvisioningDocument),
        // WHEN the device was claimed does not move because its credential
        // did, and 0 is the reading an unset clock gives.
        at: claim.at.unwrap_or(0),
        rotation_required: false,
    }) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    subtree.insert("claim".to_string(), claim);
    let mut device = subtree
        .get("device")
        .cloned()
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    device.insert("generation".to_string(), Value::from(generation));
    subtree.insert("device".to_string(), Value::Object(device));
    if let Err(err) = state
        .api
        .set_settings(ACCESS_PATH, &Value::Object(subtree))
        .await
    {
        state.audit.record(event, "aborted", &source);
        return bus_api_error(&err, Some(ACCESS_PATH));
    }

    // apid knows its own access write happened, so the gate's cache is dropped
    // here rather than waiting for the SettingsChanged round trip.
    state.access_cache.invalidate();
    // Every session was minted under a credential that no longer exists.
    state.sessions.remove_all_except("");
    if let Err(err) = state.api.admin_password_set(&secret).await {
        tracing::warn!(error = %format!("{err:#}"), "the BLE verifier was not replaced");
    }
    // The second rule, and the release path item 2 promises: a
    // SUCCESSFUL rotation clears the guard, counters and window both, which is
    // what would make a hard `lockoutThreshold` safe to ship. A refused or
    // aborted one clears nothing — every early return above leaves this line
    // unreached, which is the rule rather than a comment about it.
    state.guard.record_success();

    // AFTER the commit, and never before it: the third rule is that a
    // refused or aborted rotation clears nothing, and an assertion is a trip
    // to the device. An interrupted rotation therefore leaves the assertion
    // standing and the retry is the operator's, which is what
    // `an_interrupted_rotation_writes_nothing_and_the_retry_leaves_one_credential`
    // already required of this route.
    if let Err(err) = state.presence.spend() {
        tracing::error!(error = %err, "the presence assertion could not be spent");
    }
    state.audit.record(event, "success", &source);

    api_response(
        StatusCode::OK,
        CredentialRecovered {
            mechanism: assertion.mechanism,
            generation,
        },
    )
}

/// `access.device.generation` as the subtree holds it, or 0.
///
/// A stored value this build cannot read is 0 and therefore rotates to 1,
/// which errs towards moving the counter rather than towards a rotation that
/// left "which credential is of record" unanswerable.
pub(super) fn device_generation(access: &Value) -> u32 {
    access
        .get("device")
        .and_then(|device| device.get("generation"))
        .and_then(Value::as_u64)
        .and_then(|generation| u32::try_from(generation).ok())
        .unwrap_or(0)
}

/// A fresh administrator password: 128 bits of the system CSPRNG, hex.
///
/// Hex and not a denser alphabet because an operator reads this off a console
/// and types it into a browser, and 32 unambiguous characters beat 22 with a
/// case-and-symbol alphabet at the same entropy. Well over
/// `MIN_ADMIN_PASSWORD_LEN`, which is a floor for a password a human chose.
pub(super) fn mint_recovery_password() -> String {
    hex::encode(micad_settings::random_bytes::<16>())
}

// Login / logout

// Password change
