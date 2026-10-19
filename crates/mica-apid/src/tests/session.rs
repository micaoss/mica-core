//! Sessions: setup mode, login and CSRF.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

#[tokio::test]
pub(super) async fn session_api_reports_setup_and_authenticates_the_browser() {
    let (setup_router, _) = test_app(unconfigured_tree());
    let setup = get(&setup_router, "/api/v1/session", None).await;
    assert_eq!(setup.status(), StatusCode::OK);
    let setup: serde_json::Value =
        serde_json::from_str(&body_string(setup).await).expect("session JSON");
    assert_eq!(setup, json!({ "state": "setup" }));

    let (router, _) = test_app(configured_tree("hunter2secret"));
    let anonymous = get(&router, "/api/v1/session", None).await;
    assert_eq!(anonymous.status(), StatusCode::OK);
    let anonymous: serde_json::Value =
        serde_json::from_str(&body_string(anonymous).await).expect("session JSON");
    assert_eq!(anonymous, json!({ "state": "unauthenticated" }));

    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    assert_eq!(login.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&login);
    let login_body: serde_json::Value =
        serde_json::from_str(&body_string(login).await).expect("session JSON");
    assert_eq!(login_body["state"], "authenticated");
    let csrf = login_body["csrfToken"]
        .as_str()
        .expect("authenticated session carries CSRF token");
    assert_eq!(csrf.len(), 64);

    let authenticated = get(&router, "/api/v1/session", Some(&cookie)).await;
    assert_eq!(authenticated.status(), StatusCode::OK);
    let authenticated: serde_json::Value =
        serde_json::from_str(&body_string(authenticated).await).expect("session JSON");
    assert_eq!(authenticated, login_body);
}

#[tokio::test]
pub(super) async fn cookie_api_access_requires_csrf_only_for_mutations() {
    let (router, fake) = test_app(configured_tree("hunter2secret"));
    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    let cookie = session_cookie_value(&login);
    let login_body: serde_json::Value =
        serde_json::from_str(&body_string(login).await).expect("session JSON");
    let csrf = login_body["csrfToken"].as_str().unwrap();

    let read = get(&router, "/api/v1/meta", Some(&cookie)).await;
    assert_eq!(read.status(), StatusCode::OK);

    let missing = json_request(
        &router,
        "PUT",
        "/api/v1/settings/hostname",
        json!("new-host"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);
    let missing: serde_json::Value =
        serde_json::from_str(&body_string(missing).await).expect("API error JSON");
    assert_eq!(missing["error"]["code"], "csrf_invalid");

    let accepted = json_request(
        &router,
        "PUT",
        "/api/v1/settings/hostname",
        json!("new-host"),
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.get_settings("hostname").await.unwrap(),
        json!("new-host")
    );
}
