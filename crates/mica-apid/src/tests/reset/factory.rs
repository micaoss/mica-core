//! The factory reset: the administrator password on every request, the login
//! backoff, micad's refusals, and the trail.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

const FACTORY_RESET_PATH: &str = "/api/v1/system/factory-reset";

#[tokio::test]
pub(super) async fn the_administrator_password_stages_the_reset() {
    let (router, fake, dir, token) = device(FakePresence::absent());

    let response = bearer_json(
        &router,
        "POST",
        FACTORY_RESET_PATH,
        &token,
        &json!({ "password": PW_SENTINEL }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_api_headers(&response, FACTORY_RESET_PATH);
    assert_eq!(body_json(response).await, json!({ "applies": "reboot" }));
    let staged = fake.factory_resets();
    assert_eq!(staged.len(), 1);
    assert!(staged[0].starts_with("api "), "{staged:?}");
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("factory-reset".to_string(), "staged".to_string())]
    );
}

/// A session is no substitute for the password, and its request is the
/// console's.
#[tokio::test]
pub(super) async fn a_console_session_still_gives_the_password() {
    let (router, fake, _dir, _token) = device(FakePresence::absent());
    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": PW_SENTINEL }),
        None,
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&login);
    let csrf = body_json(login).await["csrfToken"]
        .as_str()
        .unwrap()
        .to_string();

    for body in [json!({}), json!({ "password": "" })] {
        let response = json_request(
            &router,
            "POST",
            FACTORY_RESET_PATH,
            body,
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(envelope(response).await["code"], "validation_failed");
    }
    // And no CSRF token, no reset.
    let response = json_request(
        &router,
        "POST",
        FACTORY_RESET_PATH,
        json!({ "password": PW_SENTINEL }),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(fake.factory_resets().is_empty());

    let response = json_request(
        &router,
        "POST",
        FACTORY_RESET_PATH,
        json!({ "password": PW_SENTINEL }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let staged = fake.factory_resets();
    assert!(staged[0].starts_with("console "), "{staged:?}");
}

#[tokio::test]
pub(super) async fn a_wrong_password_is_refused_and_backs_off_like_a_login() {
    let (router, fake, dir, token) = device(FakePresence::absent());

    let response = bearer_json(
        &router,
        "POST",
        FACTORY_RESET_PATH,
        &token,
        &json!({ "password": "a guess" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let envelope_of_wrong = envelope(response).await;
    assert_eq!(envelope_of_wrong["code"], "password_incorrect");
    assert_eq!(envelope_of_wrong["path"], "password");

    let response = bearer_json(
        &router,
        "POST",
        FACTORY_RESET_PATH,
        &token,
        &json!({ "password": PW_SENTINEL }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(envelope(response).await["code"], "login_throttled");
    // The login itself is backed off by the same counter.
    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": PW_SENTINEL }),
        None,
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::TOO_MANY_REQUESTS);

    assert!(fake.factory_resets().is_empty());
    let events = audit_events(&audit_lines(dir.path()));
    assert_eq!(
        events[..2],
        [
            ("factory-reset".to_string(), "wrong-password".to_string()),
            ("factory-reset".to_string(), "throttled".to_string()),
        ]
    );
}

#[tokio::test]
pub(super) async fn micad_s_refusals_are_conflicts_named_by_their_code() {
    for code in [
        "not_claimed",
        "factory_reset_unsupported",
        "update_in_progress",
        "trial_pending",
    ] {
        let (router, fake, dir, token) = device(FakePresence::absent());
        fake.set_factory_reset(json!({
            "supported": true,
            "refusal": { "code": code, "message": "not now" },
            "last": null,
        }));
        let response = bearer_json(
            &router,
            "POST",
            FACTORY_RESET_PATH,
            &token,
            &json!({ "password": PW_SENTINEL }).to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{code}");
        let envelope = envelope(response).await;
        assert_eq!(envelope["code"], code);
        assert_eq!(envelope["message"], "not now");
        assert!(fake.factory_resets().is_empty());
        assert_eq!(
            audit_events(&audit_lines(dir.path())),
            [("factory-reset".to_string(), "refused".to_string())]
        );
    }
}

#[tokio::test]
pub(super) async fn the_status_is_micad_s_and_the_routes_need_a_credential() {
    let (router, fake, _dir, token) = device(FakePresence::absent());
    let last = json!({
        "requested": 1_700_000_000,
        "completed": 1_700_000_100,
        "by": "console",
        "source": "192.0.2.7",
        "unremoved": 0,
    });
    fake.set_factory_reset(json!({ "supported": true, "refusal": null, "last": last }));

    let response = bearer(&router, "GET", FACTORY_RESET_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "supported": true, "refusal": null, "last": last })
    );

    let response = post_json(
        &router,
        FACTORY_RESET_PATH,
        &json!({ "password": PW_SENTINEL }).to_string(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(fake.factory_resets().is_empty());
}
