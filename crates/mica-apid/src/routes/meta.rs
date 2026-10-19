//! Versions, metadata and health.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use super::*;

/// `GET /api/versions` response.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ApiVersions {
    pub(super) versions: Vec<&'static str>,
    pub(super) current: &'static str,
}

/// `GET /api/v1/meta` response.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiMeta {
    pub(super) api: &'static str,
    pub(super) settings_schema_version: u32,
    pub(super) daemon: &'static str,
    /// The features this device serves, from the product it was built as:
    /// any of `wifi`, `bluetooth`, `ssh`, `containers`, `mqtt`. The routes of
    /// a feature not listed are not served and answer 404.
    pub(super) features: Vec<&'static str>,
}

/// List the API major versions this build serves.
#[utoipa::path(
    get,
    path = VERSIONS_PATH,
    context_path = API,
    tag = "discovery",
    responses(
        (status = 200, description = "The major API versions this device serves", body = ApiVersions),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_versions() -> Response {
    api_response(
        StatusCode::OK,
        ApiVersions {
            versions: SERVED_VERSIONS.to_vec(),
            current: CURRENT_VERSION,
        },
    )
}

/// Report what the caller is talking to.
///
/// Answers the API major version, the daemon name,
/// `settingsSchemaVersion` — a settings document format version, which moves
/// independently of the API version — and `features`, the parts of the API
/// this product serves. Authenticated.
///
/// **The settings tree has no single version.** Storage
/// is one document per reconciler in `/mica/config/` plus the remainder on
/// STATE, and each carries its own `schema_version` starting at v1, because a
/// namespace-wide version could not be bumped without rewriting every document
/// across renames with no transaction between them. This member therefore
/// reports the STATE document's version — the one document that is still one
/// document — and it is read live from `micad_settings` rather than copied,
/// which is the property a version read needs.
#[utoipa::path(
    get,
    path = V1_META_PATH,
    context_path = API,
    tag = "discovery",
    responses(
        (status = 200, description = "What this daemon is and which schema it speaks", body = ApiMeta),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_meta(
    State(state): State<AppState>,
    _credential: ApiCredential,
) -> Response {
    api_response(
        StatusCode::OK,
        ApiMeta {
            api: CURRENT_VERSION,
            settings_schema_version: micad_settings::STATE_SCHEMA_VERSION,
            daemon: "apid",
            features: state.features.words(),
        },
    )
}

// Optional members are omitted rather than sent null, the same rule the error
// envelope follows: a member present with a meaningless value is worse than an
// absent one.
/// The body of `GET /api/v1/health`.
///
/// `apid` and `micad` are always present. `checkedAt` is present only on the
/// reachable answer and `detail` only on the unreachable one.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiHealth {
    // On the wire so a client reads one document rather than inferring half of
    // it from the fact that a response arrived.
    /// Always `"ok"`: a request that got a body was served by an apid that is
    /// up.
    pub(super) apid: &'static str,
    // Two values and no third: a health answer that needs a taxonomy is not
    // one a monitor can act on.
    /// `"ok"` when the probe completed, `"unreachable"` when it did not.
    pub(super) micad: &'static str,
    // Uptime and not a wall-clock stamp: there is no trusted wall clock in
    // this crate, and `SessionStore` is monotonic `Instant` throughout.
    /// Whole seconds since boot, at the moment the probe answered.
    ///
    /// The same value and spelling `GET /api/v1/state/uptime` serves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) checked_at: Option<u64>,
    /// Why the probe did not complete, in the words of whatever refused it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
    // The bound on the health path. `detail` above is unbounded by
    // construction -- it is whatever zbus, the kernel or micad said -- so a
    // monitor that wanted to distinguish "micad is not there" from "micad
    // answered nonsense" had to match on that text. This is the closed
    // alternative, present exactly when `detail` is.
    /// Which class of failure the probe hit, from a fixed set:
    /// `micad_unreachable` (the call could not be made or was refused),
    /// `micad_timeout` (the bounded call expired), `micad_bad_answer` (micad
    /// replied with something that is not the documented shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) code: Option<&'static str>,
}

/// The bounded call to micad expired.
pub(super) const HEALTH_MICAD_TIMEOUT: &str = "micad_timeout";
/// The call could not be made, or micad refused it.
pub(super) const HEALTH_MICAD_UNREACHABLE: &str = "micad_unreachable";
/// micad answered, and what it answered is not the documented shape.
pub(super) const HEALTH_MICAD_BAD_ANSWER: &str = "micad_bad_answer";

// Never serve this from `access_cache`: that cache answers from apid's own
// memory, so a health route reading it would report micad healthy for as long
// as the last fill survived. `GetState("uptime")` is the probe because it is
// one call, cheap, and also yields `checkedAt`.
/// Report whether this appliance is manageable.
///
/// Answers **200 in every state**, including when micad is unreachable — a
/// failure is reported in the body, never as a status code, so it cannot be
/// confused with this endpoint being down. micad's state is determined by one
/// live bus call per request.
///
/// Authenticated. For plain listener liveness use the unauthenticated
/// `/healthz` instead; neither answer implies the other.
#[utoipa::path(
    get,
    path = V1_HEALTH_PATH,
    context_path = API,
    tag = "diagnostics",
    responses(
        (status = 200, description = "Whether this appliance is manageable. **200 in both states**: a dead micad is reported as `micad: \"unreachable\"` in the body, never as a status code. An unreachable answer carries `code` — `micad_unreachable`, `micad_timeout` or `micad_bad_answer` — beside the free-text `detail`; the code is the member to match on", body = ApiHealth),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_health(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let (micad, checked_at, detail, code) = match state.api.get_state(HEALTH_PROBE_PATH).await {
        // Any answer that is not the number of seconds micad documents is
        // classified with the failures rather than reported as health. `ok`
        // has to mean "the round trip completed and produced a usable answer";
        // a state key that came back the wrong shape did not.
        Ok(value) => match value.as_u64() {
            Some(seconds) => ("ok", Some(seconds), None, None),
            None => (
                "unreachable",
                None,
                Some(format!(
                    "micad answered GetState(\"{HEALTH_PROBE_PATH}\") with {value}, which is not a count of seconds"
                )),
                Some(HEALTH_MICAD_BAD_ANSWER),
            ),
        },
        // The code is decided from the error's TYPE, not from the sentence it
        // renders to: `MicadCallTimeout` is the one the bounded
        // call raises, and everything else is "the call did not happen".
        Err(err) => {
            let code = if err
                .downcast_ref::<crate::bus_client::MicadCallTimeout>()
                .is_some()
            {
                HEALTH_MICAD_TIMEOUT
            } else {
                HEALTH_MICAD_UNREACHABLE
            };
            ("unreachable", None, Some(format!("{err:#}")), Some(code))
        }
    };
    api_response(
        StatusCode::OK,
        ApiHealth {
            apid: "ok",
            micad,
            checked_at,
            detail,
            code,
        },
    )
}
