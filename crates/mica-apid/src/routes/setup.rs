//! The first-boot setup.

use crate::access_cache::ACCESS_PATH;
use crate::assets::mime::CacheClass;
use crate::audit::Source;
use crate::auth::{self};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, SET_COOKIE};
use axum::response::{IntoResponse, Response};
use micad_settings::{ClaimChannel, ClaimSettings, MIN_ADMIN_PASSWORD_LEN};
use serde_json::Value;

use super::*;

/// The access point's key, which the claim sets.
const AP_PSK_PATH: &str = "wifi.ap.psk";

/// `POST /api/v1/setup` request body.
///
/// No `confirm` member, unlike the form the wizard posts: that field exists so
/// a human who mistyped a password into a box they cannot read is told before
/// it becomes the only credential on the device. A client that built a JSON
/// body knows what it sent, and a second copy of the same string proves
/// nothing about it.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct SetupRequest {
    /// The admin password to set. At least 8 bytes, the floor the wizard and
    /// the change-password route enforce.
    pub(super) password: String,
    /// The hostname to apply. Absent leaves the stored one alone.
    ///
    /// Not trimmed, for the reason `PUT /api/v1/settings/hostname` gives: a
    /// client that built a JSON string chose its bytes, and silently writing
    /// something other than what it sent is the worse answer.
    #[serde(default)]
    pub(super) hostname: Option<String>,
    // Merged and not replacing: a route that replaced the map could unmake the
    // entry a factory-fresh device is reachable over.
    /// `network` entries to declare, merged into the stored map by name.
    ///
    /// An entry whose name is already declared is replaced whole; one that is
    /// not is added. Entries the body does not name are left alone.
    #[serde(default)]
    #[schema(value_type = Option<std::collections::BTreeMap<String, NetworkInterface>>)]
    pub(super) network: Option<NetworkEntries>,
    /// The access point's key from now on (`wifi.ap.psk`), on a product with
    /// Wi-Fi. Absent, the device generates one and answers it once in
    /// [`SetupResult::ap_psk`]. Either way the key derived for the label is
    /// not used after the claim (`docs/factory-ap-key.md`).
    #[serde(default, rename = "apPsk")]
    pub(super) ap_psk: Option<String>,
}

/// `POST /api/v1/setup` response body.
///
/// **No API token.** The console authenticates with the password this route
/// sets, so setup mints no long-lived bearer credential nobody asked for. A
/// caller that wants one asks for it: `POST /api/v1/tokens`, authenticated by
/// the credential this route created.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetupResult {
    /// CSRF token for the browser session created by setup.
    pub(super) csrf_token: String,
    /// The access-point key the device generated, when the request carried
    /// none: answered here once and readable nowhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) ap_psk: Option<String>,
}

/// Characters of a generated access-point key: 80 bits.
const GENERATED_AP_PSK_CHARS: usize = 16;
/// The alphabet a generated key is drawn from, the device code's: no `0`,
/// `O`, `1` or `I` to misread off a screen. 32 divides 256, so a byte folds
/// into it without bias.
const AP_PSK_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// A fresh access-point key from the system CSPRNG.
fn generate_ap_psk() -> anyhow::Result<String> {
    use aws_lc_rs::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; GENERATED_AP_PSK_CHARS];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random source failed"))?;
    Ok(bytes
        .iter()
        .map(|byte| char::from(AP_PSK_ALPHABET[usize::from(byte % 32)]))
        .collect())
}

// The write order fails safe: nothing before `access.webAdmin` takes the
// device out of setup mode, so a failure at any point leaves the wizard
// reachable. The browser wizard writes in a different order and is not changed
// here; closing that gap needs a transactional multi-path write on the bus.
/// First-run setup: set the admin password and, optionally, a hostname and
/// network entries.
///
/// **Unauthenticated**, because it creates the device's first credential.
/// Answers **409** once a password exists, which closes it permanently.
///
/// Everything is validated before anything is written; a rejected request
/// leaves the device untouched and still in setup mode. On success the writes
/// run in the order hostname, network, then `access` with `wifi.ap.psk` as
/// one write.
///
/// On a product with Wi-Fi the claim sets the access point's key: `apPsk`
/// when the body carries one, otherwise a generated key answered once as
/// `apPsk`.
///
/// Answers **201** with the browser session's CSRF token and that generated
/// key. It mints no API token: see [`SetupResult`].
#[utoipa::path(
    post,
    path = V1_SETUP_PATH,
    context_path = API,
    tag = "actions",
    request_body = SetupRequest,
    responses(
        (status = 201, description = "The device is configured and a browser session was created; the body carries that session's CSRF token", body = SetupResult),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 409, description = "The device already has an admin password, so it is not in setup mode (`already_configured`). Change the password with `POST /api/v1/actions/change-password`", body = ApiError),
        (status = 422, description = "The body is not this shape, the password is under 8 bytes, the hostname is not a hostname, a `network` key is not an interface name, a static address is not IPv4 CIDR notation, or a relational rule refuses the resulting map -- a VLAN parent or a bridge port that is not a declared entry, a bridge port carrying addressing, a port claimed twice (`validation_failed`); or micad rejected a write (`settings_rejected`). Nothing is written on any of them", body = ApiError),
        (status = 500, description = "Hashing the password failed (`hash_failed`), the stored `access` subtree could not be read (`settings_invalid`), or micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_setup(
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // No dot-path: this route writes three subtrees and a malformed body is
    // not about any one of them.
    let request: SetupRequest = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    // THE CLAIM IS ONE STEP. From here to the `access` write below is a
    // check-then-act -- the read decides the device is unclaimed, the write is
    // what claims it -- and nothing underneath makes the pair atomic: micad
    // serialises each `SetSettings` under its own write lock but offers no
    // compare-and-set, so two requests that both read an unclaimed tree both
    // write one. It is not a race that is hard to win. The window is the
    // argon2id hash below plus two bus round trips, and it was measured on the
    // shipped path with no test seam in it: two concurrent requests against a
    // real micad over a real bus produced two 201s and two working
    // administrator sessions in 200 of 200 runs, on a device that kept the
    // last password written.
    let _claim_guard = state.claim.lock().await;
    // One read of `access`: whether the device is still in setup mode. The
    // condition is `password_hash(&access).is_some` and this is that condition
    // and not a rendering of it. The subtree is kept because the write below
    // replaces it whole and must not drop what it did not write.
    let access = match state.api.get_settings("access").await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some("access")),
    };
    if password_hash(&access).is_some() {
        // A refused claim is audited, and it is the more interesting record of
        // the two: a claim can only ever succeed once, so every later attempt
        // is either an operator who lost track of a device or somebody probing
        // one. the line carries the peer address, which is the whole of what
        // makes the record useful.
        state.audit.record(CLAIM_EVENT, "refused", &source);
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "already_configured",
                "this device already has an admin password, so it is not in setup mode; \
                 change the password with `POST /api/v1/actions/change-password`"
                    .to_string(),
            )
            .at("access.webAdmin"),
        );
    }

    // Everything below this line validates. Nothing below it writes until the
    // last rule has passed and the password has been hashed. The divergence
    // from the wizard that this creates is asserted by
    // `the_api_setup_route_validates_before_writing_where_the_form_path_does_not`,
    // which drives the same invalid request through both surfaces.
    if password_under_floor(&request.password) {
        // The bound and never the password: the message names how long it must
        // be and interpolates nothing the caller sent.
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!("the admin password must be at least {MIN_ADMIN_PASSWORD_LEN} bytes"),
            )
            .at("access.webAdmin"),
        );
    }
    if let Some(hostname) = request.hostname.as_deref()
        && !valid_hostname(hostname)
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", HOSTNAME_RULES.to_string()).at("hostname"),
        );
    }
    let wifi = state.features.has(micad_settings::Feature::Wifi);
    if let Some(psk) = request.ap_psk.as_deref() {
        let refusal = if wifi {
            micad_settings::validate_wifi_psk(psk).err()
        } else {
            Some("this product has no Wi-Fi access point".to_string())
        };
        if let Some(message) = refusal {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", message).at(AP_PSK_PATH),
            );
        }
    }
    // The candidate tree and not the submitted entries, for the reason the
    // pane's comment gives and the network routes act on: every relational rule is
    // about two entries at once, so a submitted bridge port may legitimately
    // name an interface the device already declares. The validators are the
    // network routes' own, called and not copied.
    let candidate = match request.network.as_ref() {
        None => None,
        Some(submitted) => {
            for (iface, cfg) in submitted {
                if let Err(response) = check_iface_name(iface, NETWORK_SETTINGS_PATH) {
                    return *response;
                }
                // The rest of the wizard's entry validator, called and not
                // copied. Its name branch cannot fire here -- `check_iface_name`
                // above tests the same predicate -- so the only message it can
                // produce is the CIDR one, which is a rule that lives in a
                // function whose only two callers are HTML form handlers and is
                // therefore run by no route under `/api/v1/`. Called here
                // because a factory-fresh device configured with an address the
                // kernel cannot parse is exactly the unreachable box this
                // check exists to prevent. That the network routes do not
                // call it is a finding on its own, not something this route may
                // fix on their behalf.
                let address = cfg
                    .static_
                    .as_ref()
                    .map_or("", |static_| static_.address.as_str());
                if let Err(message) = validate_iface(iface, cfg.dhcp, address) {
                    return api_response(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        ApiError::apid("validation_failed", message.to_string())
                            .at(NETWORK_SETTINGS_PATH),
                    );
                }
            }
            let mut candidate = match api_network_entries(&state).await {
                Ok(entries) => entries,
                Err(response) => return *response,
            };
            for (iface, cfg) in submitted {
                candidate.insert(iface.clone(), cfg.clone());
            }
            if let Err(response) = relational_refusal(&candidate, NETWORK_SETTINGS_PATH) {
                return *response;
            }
            Some(candidate)
        }
    };
    // Off the async workers for the same reason login verification and the
    // wizard's own hash are: argon2id costs real CPU per call, by design. It
    // is done here, before the first write, because it is the last step that
    // can fail without the caller having asked for anything impossible.
    let password = request.password.clone();
    let hash = match tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .unwrap_or_else(|err| Err(anyhow::anyhow!("password hashing task: {err}")))
    {
        Ok(hash) => hash,
        Err(err) => {
            // Logged, not returned: the error carries argon2's own text and
            // the caller can do nothing with it.
            tracing::error!(error = %err, "password hashing failed");
            return api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "hash_failed",
                    "the admin password could not be hashed; nothing was written".to_string(),
                )
                .at("access.webAdmin"),
            );
        }
    };

    let generated_ap_psk = match (wifi, &request.ap_psk) {
        (true, None) => match generate_ap_psk() {
            Ok(psk) => Some(psk),
            Err(err) => {
                tracing::error!(error = %err, "access-point key generation failed");
                return api_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::apid(
                        "hash_failed",
                        "no access-point key could be generated; nothing was written".to_string(),
                    )
                    .at(AP_PSK_PATH),
                );
            }
        },
        _ => None,
    };

    // The writes. `hostname` and `network` come first and are their own calls:
    // neither takes the device out of the unclaimed state, so a failure in
    // either leaves an unclaimed, still-claimable device and the caller may
    // simply post the same body again.
    if let Some(hostname) = request.hostname.as_deref()
        && let Err(err) = state
            .api
            .set_settings("hostname", &Value::String(hostname.to_string()))
            .await
    {
        return bus_api_error(&err, Some("hostname"));
    }
    if let Some(candidate) = candidate.as_ref()
        && let Err(response) = write_network_map(&state, candidate).await
    {
        // The whole map in one call and not one call per entry, so N submitted
        // interfaces are still one write and not N chances to half-apply.
        return *response;
    }

    // The claim itself, as ONE write of the whole `access` subtree. Any token
    // list already in that subtree is carried through untouched: this route
    // writes the password and the claim record, and nothing about tokens.
    let claim = ClaimSettings {
        via: ClaimChannel::Setup,
        at: device_clock_seconds(),
        // A password the caller chose at this moment and posted over the
        // management API is not a bootstrap secret: it was never written onto
        // a medium and never left with a device.
        rotation_required: false,
    };
    // A subtree that is not an object is a tree no `Settings` produced, so
    // there is nothing in it to preserve; an empty map is what this route
    // would have written into anyway.
    let mut subtree = match access {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    subtree.insert(
        "webAdmin".to_string(),
        serde_json::json!({ "password_hash": hash }),
    );
    let claim = match encode(claim) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    subtree.insert("claim".to_string(), claim);
    // The access point's key lands with the claim and not beside it: either
    // order of two writes leaves, on a failure between them, a device whose
    // access point runs a key nobody was shown.
    let written = match request.ap_psk.clone().or_else(|| generated_ap_psk.clone()) {
        Some(psk) => {
            state
                .api
                .set_settings_many(&[
                    (ACCESS_PATH.to_string(), Value::Object(subtree)),
                    (AP_PSK_PATH.to_string(), Value::String(psk)),
                ])
                .await
        }
        None => {
            state
                .api
                .set_settings(ACCESS_PATH, &Value::Object(subtree))
                .await
        }
    };
    if let Err(err) = written {
        return bus_api_error(&err, Some(ACCESS_PATH));
    }
    // The device just left setup mode, and the gate must not keep believing
    // otherwise from a cached pre-write snapshot.
    state.access_cache.invalidate();
    // Recorded once the credential exists, which is the moment the device is
    // claimed. The audit trail says what happened to the device, not which surface
    // asked, so the event names the transition and not the route.
    state.audit.record(CLAIM_EVENT, "completed", &source);
    let session = state.sessions.create();
    (
        StatusCode::CREATED,
        [
            (
                CACHE_CONTROL,
                CacheClass::NoStore.header_value().to_string(),
            ),
            (SET_COOKIE, state.session_cookie(&session.cookie)),
        ],
        Json(SetupResult {
            csrf_token: session.csrf_token,
            ap_psk: generated_ap_psk,
        }),
    )
        .into_response()
}

// The claim lifecycle
