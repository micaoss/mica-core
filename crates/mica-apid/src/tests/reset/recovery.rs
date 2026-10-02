//! Credential recovery: minting, publishing and refusing.

use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use super::*;

/// The flow mints, publishes once, invalidates at the same commit and bumps
/// the generation — and the commit is ONE write.
///
/// Every clause of the recovery contract is one assertion below. The write count is the one
/// that makes the rest hold: the new hash, the emptied token list, the claim
/// record and the generation are four keys of one subtree, so no crash between
/// two writes can leave a device whose old credential was invalidated and
/// whose new one was not stored.
#[tokio::test]
pub(super) async fn recovery_mints_publishes_once_and_invalidates_at_the_same_commit() {
    let presence = FakePresence::present();
    let (router, fake, dir, token) = device(presence.clone());
    let before = access_of(&fake).await;
    let previous_hash = before["webAdmin"]["password_hash"]
        .as_str()
        .unwrap()
        .to_string();

    let response = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, RECOVERY_PATH);
    let body = body_json(response).await;
    assert_eq!(body["mechanism"], json!(FIXTURE_MECHANISM));
    assert_eq!(body["generation"], json!(5), "the generation did not move");

    assert_eq!(
        fake.set_paths(),
        vec!["access".to_string()],
        "the rotation must be one write of one subtree"
    );
    let access = access_of(&fake).await;

    // MINTED, and the previous secret is invalidated at that same commit.
    let hash = access["webAdmin"]["password_hash"].as_str().unwrap();
    assert_ne!(hash, previous_hash, "the credential was not replaced");
    // Every API token goes with it.
    assert_eq!(access["apiTokens"], json!([]));
    let stale = bearer(&router, "GET", "/api/v1/meta", &token).await;
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    // The claim record says the credential of record is no longer a bootstrap
    // secret, and WHEN the device was claimed has not moved.
    assert_eq!(access["claim"]["via"], json!("setup"));
    assert_eq!(access["claim"]["rotationRequired"], json!(false));
    assert_eq!(access["claim"]["at"], json!(1_700_000_000_u64));

    // PUBLISHED exactly once, on the channel that proved presence, and it is
    // the credential that now works.
    let secret = presence.secret();
    let session = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": secret }),
        None,
        None,
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    // And the previous one does not.
    let refused = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": PW_SENTINEL }),
        None,
        None,
    )
    .await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);

    assert!(
        audit_events(&audit_lines(dir.path())).contains(&(
            "credential-recovery-boot-menu".to_string(),
            "success".to_string()
        )),
        "{:?}",
        audit_events(&audit_lines(dir.path()))
    );
}

/// Presence is the authority. Without it the flow is refused, nothing is
/// published, nothing is written, and the refusal is audited under the same
/// event name — rule 4's "a refused attempt is the more interesting
/// record".
#[tokio::test]
pub(super) async fn recovery_is_refused_without_presence_and_publishes_nothing() {
    let presence = FakePresence::absent();
    let (router, fake, dir, _token) = device(presence.clone());

    let response = post_json(&router, RECOVERY_PATH, "", None).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(envelope(response).await["code"], "presence_required");
    assert!(presence.published().is_empty());
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("credential-recovery".to_string(), "refused".to_string())]
    );
}

/// **An authenticated session may not run it**, and holding a credential does
/// not become an authority by standing at the device: presence is asserted
/// here and the caller is still refused.
///
/// The argument, asserted: a session that can rotate the credential it
/// authenticated with is a session-fixation lever, and the operator holding a
/// working credential needs the ordinary change-password path.
#[tokio::test]
pub(super) async fn an_authenticated_session_cannot_run_credential_recovery() {
    let presence = FakePresence::present();
    let (router, fake, dir, token) = device(presence.clone());
    let cookie = login(&router, PW_SENTINEL).await;

    // The bearer client.
    let by_token = bearer_json(&router, "POST", RECOVERY_PATH, &token, "").await;
    assert_eq!(by_token.status(), StatusCode::FORBIDDEN);
    let refusal = envelope(by_token).await;
    assert_eq!(refusal["code"], "authenticated_session");
    assert!(
        refusal["message"]
            .as_str()
            .is_some_and(|message| message.contains("change-password")),
        "{refusal}"
    );

    // The browser session.
    let by_session = post_json(&router, RECOVERY_PATH, "", Some(&cookie)).await;
    assert_eq!(by_session.status(), StatusCode::FORBIDDEN);
    assert_eq!(envelope(by_session).await["code"], "authenticated_session");

    assert!(presence.published().is_empty());
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    let events = audit_events(&audit_lines(dir.path()));
    assert_eq!(
        events
            .iter()
            .filter(|(event, outcome)| event == "credential-recovery" && outcome == "refused")
            .count(),
        2,
        "{events:?}"
    );
}

/// A device with no credential has nothing to recover, and this flow does not
/// become a third channel that can claim one.
#[tokio::test]
pub(super) async fn recovery_refuses_an_unclaimed_device_and_points_at_setup() {
    let dir = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(unconfigured_tree()));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY)
        .with_persistence(dir.path())
        .with_presence(FakePresence::present()));

    let response = post_json(&router, RECOVERY_PATH, "", None).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let refusal = envelope(response).await;
    assert_eq!(refusal["code"], "not_claimed");
    assert!(
        refusal["message"]
            .as_str()
            .is_some_and(|message| message.contains("/api/v1/setup")),
        "{refusal}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

/// The rotation bound, both halves, asserted through the behaviour the guard has: a
/// SUCCESSFUL rotation clears the counters and the window, and a REFUSED one
/// clears nothing.
///
/// The observation is the login route's own answer. A guard with an armed
/// window turns a login away with 429 before it looks at the password, so
/// "the window is gone" is exactly "a correct password is admitted again" —
/// which is the release path item 2 promises and what would make a hard
/// `lockoutThreshold` survivable.
#[tokio::test]
pub(super) async fn a_successful_rotation_clears_the_login_guard_and_a_refused_one_does_not() {
    // The refused rotation first, on its own device: it must leave the armed
    // window exactly as it found it.
    let (router, _fake, dir, _token) = device(FakePresence::absent());
    let armed = arm_the_guard(&router, dir.path()).await;
    let refused = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        guard_state(dir.path()),
        armed,
        "a refused rotation moved the guard: it must touch neither the failure run nor the \
         armed window"
    );

    // The successful one, on a device whose guard is armed the same way.
    let presence = FakePresence::present();
    let (router, _fake, dir, _token) = device(presence.clone());
    arm_the_guard(&router, dir.path()).await;
    let recovered = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(
        recovered.status(),
        StatusCode::OK,
        "the rotation was throttled by the guard it is not subject to"
    );
    assert_eq!(
        guard_state(dir.path()),
        json!({ "failures": 0, "locked_until_unix": 0 }),
        "a successful rotation left the guard armed"
    );
    let session = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": presence.secret() }),
        None,
        None,
    )
    .await;
    assert_eq!(
        session.status(),
        StatusCode::CREATED,
        "the rotated credential was refused"
    );
}

/// The login-guard state `GuardStore` persisted, as JSON.
///
/// The guard is read here rather than probed with a second login request.
/// A probe asserts "still throttled", which is only true while the window is
/// armed — `BACKOFF_BASE` is one second, so every request that had to land
/// inside it was a race against the machine's load rather than an assertion
/// about the rotation. The file says what the guard holds, whenever it is
/// read.
pub(super) fn guard_state(state_dir: &std::path::Path) -> serde_json::Value {
    let path = state_dir.join("login_guard.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|err| panic!("the guard state at {} is unreadable: {err}", path.display()));
    serde_json::from_slice(&bytes).expect("the guard state is JSON")
}

/// Arm the login guard with a wrong password, and return the state it armed
/// so a caller can require that state to be exactly what it finds later.
pub(super) async fn arm_the_guard(
    router: &Router,
    state_dir: &std::path::Path,
) -> serde_json::Value {
    let wrong = json_request(
        router,
        "POST",
        "/api/v1/session",
        json!({ "password": "not-the-password" }),
        None,
        None,
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let armed = guard_state(state_dir);
    assert_eq!(
        armed["failures"], 1,
        "the wrong password charged no failure, so the guard is not armed and the caller \
         asserts nothing"
    );
    assert_ne!(
        armed["locked_until_unix"], 0,
        "the charged failure armed no window, so the guard is not armed and the caller \
         asserts nothing"
    );
    armed
}

/// The publication is attempted BEFORE the commit, so a console that cannot be
/// written leaves the previous credential working and nothing on STATE.
///
/// Rule 3's direction: the alternative ordering — invalidate, then fail
/// to publish — is a self-inflicted lockout with no way back except a factory
/// reset.
#[tokio::test]
pub(super) async fn a_rotation_that_cannot_publish_is_aborted_and_writes_nothing() {
    let presence = FakePresence::present_but_unwritable();
    let (router, fake, dir, token) = device(presence.clone());
    let before = access_of(&fake).await;

    let response = post_json(&router, RECOVERY_PATH, "", None).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(envelope(response).await["code"], "publish_failed");
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(access_of(&fake).await, before);
    // The previous credential still works, which is the whole point of the
    // ordering.
    let probe = bearer(&router, "GET", "/api/v1/meta", &token).await;
    assert_eq!(probe.status(), StatusCode::OK);
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [(
            "credential-recovery-boot-menu".to_string(),
            "aborted".to_string()
        )]
    );
}

// --- Power loss, and the retry ---------------------------------------------
