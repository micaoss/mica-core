//! The bootstrap claim, the rotation it binds and the factory-fresh device.

use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use super::*;

/// A claim by route and a claim by document answer the SAME projection, with
/// the channel and the moment each of them can honestly name.
///
/// There is one route that answers "how was this device claimed", and it
/// answers for both. The document channel writes no record of its own — micad's
/// importer is the one writer that does not — and is read off the evidence the
/// import already persists, so there is no second field that could disagree with
/// `access.webAdmin` about whether the device is claimed.
#[tokio::test]
pub(super) async fn a_document_claim_and_a_route_claim_answer_one_projection() {
    // Claimed by the route.
    let (router, fake) = test_app(seeded_tree());
    let created = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&created);
    let by_route = body_json(get(&router, CLAIM_PATH, Some(&cookie)).await).await;

    assert_eq!(by_route["state"], json!("claimed"));
    assert_eq!(by_route["via"], json!("setup"));
    assert_eq!(by_route["rotationRequired"], json!(false));
    assert_eq!(
        by_route["at"],
        access_of(&fake).await["claim"]["at"],
        "the route's answer must be the stored record and not a second reading"
    );

    // Claimed by a document, on a tree that carries no claim record at all.
    let (tree, token) = with_token(document_claimed_tree("hunter2secret"));
    let (router, _fake) = test_app(tree);
    let by_document = body_json(bearer(&router, "GET", CLAIM_PATH, &token).await).await;

    assert_eq!(by_document["state"], json!("claimed"));
    assert_eq!(by_document["via"], json!("provisioning-document"));
    assert_eq!(by_document["rotationRequired"], json!(true));
    // WHEN comes from the import record, not from a copy.
    assert_eq!(by_document["at"], json!(1_700_000_000_u64));
}

/// The route needs a credential, and a device carrying a token but no
/// administrator credential is `unclaimed` rather than a claim with no channel.
#[tokio::test]
pub(super) async fn the_claim_route_needs_a_credential_and_can_report_an_unclaimed_device() {
    let (tree, token) = with_token(seeded_tree());
    let (router, _fake) = test_app(tree);

    let anonymous = get(&router, CLAIM_PATH, None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(envelope(anonymous).await["code"], "not_authenticated");

    let response = bearer(&router, "GET", CLAIM_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, CLAIM_PATH);
    let status = body_json(response).await;
    assert_eq!(status["state"], json!("unclaimed"));
    assert_eq!(status["rotationRequired"], json!(false));
    assert!(status.get("via").is_none(), "{status}");
}

// --- The bound: forced rotation --------------------------------------------

/// The bound bites on every authenticated mutation, and on exactly two things
/// it must not: reads, and the password change that clears it.
///
/// That pair of exemptions is what makes the bound unable to brick a device.
/// The operator holding the bootstrap credential can always sign in, can
/// always read WHY they were refused, and can always do the one thing that
/// discharges it.
#[tokio::test]
pub(super) async fn a_bootstrap_claim_refuses_every_mutation_but_the_rotation() {
    let (tree, token) = with_token(document_claimed_tree("hunter2secret"));
    let (router, _fake) = test_app(tree);

    // Reads are open, including the one that says why.
    let status = bearer(&router, "GET", CLAIM_PATH, &token).await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(body_json(status).await["rotationRequired"], json!(true));
    assert_eq!(
        bearer(&router, "GET", "/api/v1/settings/hostname", &token)
            .await
            .status(),
        StatusCode::OK
    );

    // Signing in and out are open: a bound that locked the operator out of
    // their own device would be the brick this design exists to avoid, and a
    // logout the device refuses would be a rule about the credential forcing a
    // live session to stay open.
    let session = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&session);
    let csrf = body_json(session).await["csrfToken"]
        .as_str()
        .unwrap()
        .to_string();
    let logout = json_request(
        &router,
        "DELETE",
        "/api/v1/session",
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    // Every mutation is refused, with the same stable code and no write.
    for (method, path, body) in [
        ("POST", "/api/v1/tokens", json!({ "name": "ci" })),
        ("PUT", "/api/v1/settings/hostname", json!("renamed")),
        ("POST", "/api/v1/actions/reboot", json!({})),
        ("PUT", "/api/v1/network", json!({})),
    ] {
        let response = bearer_json(&router, method, path, &token, &body.to_string()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{method} {path}");
        let envelope = envelope(response).await;
        assert_eq!(envelope["code"], "rotation_required", "{method} {path}");
        assert_eq!(envelope["path"], json!("access.claim"), "{method} {path}");
    }
}

/// The rotation discharges the bound, in ONE write, and is audited under its
/// own event name.
///
/// The write count is the assertion that matters: the new hash and the record
/// that the bootstrap secret is gone commit together, so no crash between two
/// writes can leave a device excused from a rotation that never landed.
#[tokio::test]
pub(super) async fn the_rotation_discharges_the_bound_in_one_write_and_is_audited() {
    let dir = TempDir::new().unwrap();
    let (tree, token) = with_token(document_claimed_tree("hunter2secret"));
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_persistence(dir.path()));

    let response = bearer_json(
        &router,
        "POST",
        CHANGE_PASSWORD_PATH,
        &token,
        &json!({
            "currentPassword": "hunter2secret",
            "newPassword": PW_SENTINEL,
        })
        .to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    assert_eq!(
        fake.set_paths(),
        vec!["access".to_string()],
        "the rotation must be one write of one subtree"
    );
    let access = access_of(&fake).await;
    assert_eq!(access["claim"]["via"], json!("provisioning-document"));
    assert_eq!(access["claim"]["rotationRequired"], json!(false));
    assert_eq!(
        access["claim"]["at"],
        json!(1_700_000_000_u64),
        "rotating a credential does not move WHEN the device was claimed"
    );

    // The bound is gone: the mutation that was refused now lands.
    let mint = bearer_json(
        &router,
        "POST",
        "/api/v1/tokens",
        &token,
        &json!({ "name": "ci" }).to_string(),
    )
    .await;
    assert_eq!(mint.status(), StatusCode::CREATED);

    let events = audit_events(&audit_lines(dir.path()));
    assert!(
        events.contains(&("claim-rotation".to_string(), "completed".to_string())),
        "{events:?}"
    );
    let raw = std::fs::read_to_string(dir.path().join("audit.log")).unwrap();
    assert!(!raw.contains(PW_SENTINEL), "{raw}");
}

/// A refused mutation is audited too, under the rotation's own event name.
#[tokio::test]
pub(super) async fn a_mutation_refused_for_want_of_a_rotation_is_audited() {
    let dir = TempDir::new().unwrap();
    let (tree, token) = with_token(document_claimed_tree("hunter2secret"));
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_persistence(dir.path()));

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/tokens",
        &token,
        &json!({ "name": "ci" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("claim-rotation".to_string(), "refused".to_string())]
    );
    assert!(
        fake.set_paths().is_empty(),
        "a refused mutation wrote: {:?}",
        fake.set_paths()
    );
}

/// A claim by route needs no rotation, so the bound never fires on the
/// channel whose credential the caller chose.
///
/// The mint is also where the first API token of a route-claimed device comes
/// from now that setup makes none: the session setup handed back is what asks
/// for it.
#[tokio::test]
pub(super) async fn a_route_claim_is_not_bound_by_the_rotation() {
    let (router, _fake) = test_app(seeded_tree());
    let created = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&created);
    let csrf = body_json(created).await["csrfToken"]
        .as_str()
        .expect("setup hands back the session's CSRF token")
        .to_string();

    let mint = json_request(
        &router,
        "POST",
        "/api/v1/tokens",
        json!({ "name": "ci" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(mint.status(), StatusCode::CREATED);
}

// --- One device, one owner --------------------------------------------------

/// **A factory-fresh device must not issue two administrator sessions to
/// concurrent claimants** — and the claimant that loses must be told so.
///
/// The claim is a check-then-act. `POST /api/v1/setup` reads `access` to
/// decide the device is still unclaimed, and writes `access` to claim it;
/// between the two sit the validators, the argon2id hash and the optional
/// hostname and network writes. micad serialises each `SetSettings` under its
/// own write lock but offers no compare-and-set, so nothing below apid makes
/// that pair one step: without apid's guard, two requests that both read an
/// unclaimed tree both write one, the second silently replacing the first
/// administrator's credential while apid hands each of them a session.
///
/// [`FakeSettings::hold_access_reads`] makes that interleaving deterministic
/// here by holding the first two `access` reads until both have been taken.
/// **It widens the window; it does not create one** — the window is the work
/// the route does between its own read and its own write, and it is wide
/// enough to lose without any help at all. The hold is time-bounded because an
/// atomic claim makes the second read unreachable until the first request has
/// finished: reaching that bound is the property holding, not a hung test.
#[tokio::test]
pub(super) async fn a_factory_fresh_device_issues_one_administrator_session_to_concurrent_claimants()
 {
    const FIRST_PASSWORD: &str = "first-claimant-password";
    const SECOND_PASSWORD: &str = "second-claimant-password";

    let (router, fake) = test_app(seeded_tree());
    fake.hold_access_reads(2, std::time::Duration::from_secs(1));

    let first_body = json!({ "password": FIRST_PASSWORD }).to_string();
    let second_body = json!({ "password": SECOND_PASSWORD }).to_string();
    let (first, second) = tokio::join!(
        post_json(&router, "/api/v1/setup", &first_body, None),
        post_json(&router, "/api/v1/setup", &second_body, None),
    );

    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CREATED)
            .count(),
        1,
        "A factory-fresh device must not issue two administrator sessions to \
         concurrent claimants; the two claims answered {statuses:?}"
    );

    // Which request won is the scheduler's business and not this test's; which
    // password the device ends up holding is not.
    let first_won = first.status() == StatusCode::CREATED;
    let (winner, loser) = if first_won {
        (first, second)
    } else {
        (second, first)
    };
    let (winning_password, losing_password) = if first_won {
        (FIRST_PASSWORD, SECOND_PASSWORD)
    } else {
        (SECOND_PASSWORD, FIRST_PASSWORD)
    };

    // The loser's outcome, which is the half a count of 201s does not state.
    // A status an operator can act on -- the same refusal a later claim gets,
    // naming the path and the route that changes a password -- and no session:
    // a claim that was not granted must not leave a cookie behind that reads
    // the appliance.
    assert_eq!(
        loser.status(),
        StatusCode::CONFLICT,
        "the losing claimant must be refused, not left to guess"
    );
    assert!(
        loser.headers().get(SET_COOKIE).is_none(),
        "the losing claim issued a session cookie: {:?}",
        loser.headers().get(SET_COOKIE)
    );
    let refusal = envelope(loser).await;
    assert_eq!(refusal["code"], "already_configured");
    assert_eq!(refusal["path"], "access.webAdmin");

    // One claim reached the device, and it is the winner's. The write list and
    // not the tree, for `a_route_claim_commits_...`'s reason: a tree written
    // twice looks exactly like a tree written once.
    assert_eq!(
        fake.set_paths(),
        vec!["access".to_string()],
        "a second claim reached the device"
    );
    let access = access_of(&fake).await;
    assert!(
        access.get("apiTokens").is_none(),
        "a claim left a bearer credential behind: {access}"
    );
    assert_eq!(access["claim"]["via"], json!("setup"));

    // The winner owns the device: its session reads protected settings.
    let cookie = session_cookie_value(&winner);
    let read = get(&router, "/api/v1/settings/hostname", Some(&cookie)).await;
    assert_eq!(
        read.status(),
        StatusCode::OK,
        "the granted claim's session cannot read the device it claimed"
    );

    // And the loser owns nothing. It holds no session, so the only thing it
    // can present is none -- and the password it posted never became the
    // device's credential, which is the assertion a last-write-wins claim
    // fails even when it hands out a single cookie. The winning login goes
    // first because a failed one arms the backoff.
    let anonymous = get(&router, "/api/v1/settings/hostname", None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let granted = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": winning_password }),
        None,
        None,
    )
    .await;
    assert_eq!(
        granted.status(),
        StatusCode::CREATED,
        "the credential the device kept is not the one the granted claim set"
    );
    let refused = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": losing_password }),
        None,
        None,
    )
    .await;
    assert_eq!(
        refused.status(),
        StatusCode::UNAUTHORIZED,
        "the refused claimant's password became a credential on the device"
    );
}
