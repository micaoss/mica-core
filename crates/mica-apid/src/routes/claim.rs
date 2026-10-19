//! Claiming the device.

use crate::access_cache::ACCESS_PATH;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::{ClaimChannel, ClaimSettings};
use serde_json::Value;

use super::*;

/// The audit event name for the unclaimed → claimed transition.
///
/// Named for the transition rather than for the route, because both channels
/// that can cause it produce the same state and the trail says what happened
/// to the device. Its outcomes are `completed` and `refused`, which is the
/// grammar `login` and `custom-ui-upload` already use; the ROTATION that bounds
/// a bootstrap claim is a different action and carries its own name
/// ([`CLAIM_ROTATION_EVENT`]), the way `update-mark` and `update-rollback` are
/// two names rather than one with two outcomes.
pub(super) const CLAIM_EVENT: &str = "claim";

/// The audit event name for the rotation that discharges a bootstrap claim.
///
/// Outcomes: `completed` when the bootstrap credential is replaced, `refused`
/// when a mutation is turned away because it has not been.
pub(super) const CLAIM_ROTATION_EVENT: &str = "claim-rotation";

/// Whether a mutation on `path` is one the forced rotation holds back.
///
/// Two exemptions, and they are the whole list. Both are mutations that write
/// nothing to the device:
///
/// - `POST /api/v1/actions/change-password` is the rotation itself, so holding
///   it back would make the bound unclearable;
/// - `/v1/session` is login and logout. A session lives in apid's memory and
///   is a fact about a browser, not about the appliance. An operator who
///   cannot sign in has no way to reach the exemption above, and one who
///   cannot sign OUT is being made to keep a live session by a rule that
///   exists to protect the credential behind it.
///
/// Stated as an exemption list rather than as an enforcement list on purpose:
/// a route added later is bound by default, which is the direction that fails
/// safe.
pub(super) fn bound_by_rotation(path: &str) -> bool {
    path != V1_CHANGE_PASSWORD_PATH && path != V1_SESSION_PATH
}

/// The claim record stored under `access.claim`, when the subtree carries one
/// this build can read.
///
/// A record that does not parse is `None` and therefore reads as a claim by
/// provisioning document, which is the fail-safe direction: the consequence is
/// that a rotation is asked for, never that one is excused.
pub(super) fn stored_claim(access: &Value) -> Option<ClaimSettings> {
    serde_json::from_value(access.get("claim")?.clone()).ok()
}

/// `GET /api/v1/claim` response body.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaimStatus {
    /// `unclaimed` or `claimed`.
    pub(super) state: &'static str,
    /// Which channel minted the administrator credential: `setup` or
    /// `provisioning-document`. Absent while the device is unclaimed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub(super) via: Option<ClaimChannel>,
    /// The device clock's reading when the claim committed, seconds since the
    /// UNIX epoch. **A label, never a deadline** — the reading
    /// `access.apiTokens[].created` is. Absent while the device is unclaimed,
    /// and absent for a claim by document that predates any recorded import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) at: Option<u64>,
    /// Whether the claiming credential is still the bootstrap secret it
    /// arrived as. While this is true the device serves reads and refuses
    /// every authenticated mutation but the password change that clears it.
    pub(super) rotation_required: bool,
}

/// The claim, projected from the settings tree.
///
/// **One truth, read two ways.** `access.webAdmin` is what makes a device
/// claimed and always was; this adds the part it cannot state. A claim through
/// `POST /api/v1/setup` writes `access.claim` in the same save as the
/// credential, so its record is read back verbatim. A claim by provisioning
/// document writes no record — micad's importer is the one writer that does not,
/// deliberately — and is recognised by the absence:
///
/// - only two writers can create the FIRST `access.webAdmin` on a device that
///   has none, this route and the importer, because every other writer of that
///   path is authenticated and an unclaimed device has no credential to
///   authenticate with;
/// - the importer refuses to apply a document at all once `access.webAdmin`
///   exists, so if a document applied
///   AND a credential exists AND no record does, that document is what created
///   it.
///
/// The `provisioning` subtree is read only in that case, and it is exactly the
/// case whose mutations are about to be refused, so the ordinary claimed
/// device pays no second bus call. A `provisioning` read that FAILS answers
/// "not claimed by document": the failure direction here is open, for
/// the reason — the alternative to a wrong guess is
/// an appliance no operator can reach.
pub(super) async fn claim_status(state: &AppState, access: &Value) -> ClaimStatus {
    if password_hash(access).is_none() {
        return ClaimStatus {
            state: "unclaimed",
            via: None,
            at: None,
            rotation_required: false,
        };
    }
    if let Some(claim) = stored_claim(access) {
        return ClaimStatus {
            state: "claimed",
            via: Some(claim.via),
            at: Some(claim.at),
            rotation_required: claim.rotation_required,
        };
    }
    let document = match state.api.get_settings("provisioning").await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "reading `provisioning` for the claim record failed");
            return ClaimStatus {
                state: "claimed",
                via: None,
                at: None,
                rotation_required: false,
            };
        }
    };
    let applied = document
        .get("document")
        .and_then(|record| record.get("appliedDigest"))
        .and_then(Value::as_str)
        .is_some();
    if !applied {
        // Claimed, with no record and no applied document. Nothing this
        // build writes produces that tree, so there is nothing to say about
        // the channel and nothing to demand a rotation of.
        return ClaimStatus {
            state: "claimed",
            via: None,
            at: None,
            rotation_required: false,
        };
    }
    ClaimStatus {
        state: "claimed",
        via: Some(ClaimChannel::ProvisioningDocument),
        // The import record this reads holds WHEN; the claim does not copy it
        // into a second field that could disagree.
        at: document
            .get("document")
            .and_then(|record| record.get("lastImport"))
            .and_then(|import| import.get("at"))
            .and_then(Value::as_u64),
        rotation_required: true,
    }
}

/// Whether the credential authenticating this request is a bootstrap secret
/// that still has to be rotated.
pub(super) async fn rotation_required(state: &AppState) -> bool {
    let access = match access_settings(state).await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "reading `access` for the rotation gate failed");
            return false;
        }
    };
    claim_status(state, &access).await.rotation_required
}

/// Report how this device was claimed and whether its credential must still be
/// rotated.
///
/// **Authenticated, deliberately.** `GET /api/v1/session` already tells an
/// unauthenticated caller whether the device is in setup mode, and that is all
/// an unauthenticated caller learns here too — "this device was claimed from a
/// medium and is still holding the password that was on it" is a sentence an
/// attacker would act on, and the operator who needs to read it is signed in
/// by construction.
///
/// This is the surface that makes the bound observable BEFORE it bites: the
/// operator who claimed a device with a provisioning document sees
/// `rotationRequired` here, and can see it the moment they sign in rather than
/// on the first mutation the device turns away.
#[utoipa::path(
    get,
    path = V1_CLAIM_PATH,
    context_path = API,
    tag = "session",
    responses(
        (status = 200, description = "How the device was claimed and whether its credential must be rotated", body = ClaimStatus),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The access settings could not be read", body = ApiError),
        (status = 503, description = "micad is unavailable (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_claim(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let access = match state.api.get_settings(ACCESS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(ACCESS_PATH)),
    };
    api_response(StatusCode::OK, claim_status(&state, &access).await)
}

// Physical presence, the reset tiers and credential recovery
