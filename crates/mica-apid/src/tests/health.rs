//! Health, and wrong methods on declared routes.

use crate::settings_api::FakeSettings;
use axum::Router;
use axum::http::StatusCode;
use axum::http::header::{ALLOW, CONTENT_TYPE, LOCATION, RETRY_AFTER};
use serde_json::json;
use std::sync::Arc;

use super::*;

// A tree with an admin password, and a live-state tree carrying the `uptime`
// key micad serves at read time.
pub(super) fn health_app(uptime: u64) -> (Router, Arc<FakeSettings>) {
    let (router, fake) = test_app(configured_tree("hunter2secret"));
    fake.set_state_entry("uptime", json!(uptime));
    (router, fake)
}

// The same, with a bearer token seeded into the tree: the route is
// authenticated, so a test that reads its body needs a credential.
pub(super) fn health_app_with_token(
    uptime: serde_json::Value,
) -> (Router, Arc<FakeSettings>, String) {
    let (tree, wire) = with_token(configured_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("uptime", uptime);
    (router, fake, wire)
}

// The first health shape, exactly: `apid` ok, `micad` ok, and `checkedAt`
// carrying the appliance's uptime.

// The case the route exists for: micad is dead and the answer is still **200**.
#[tokio::test]
pub(super) async fn health_reports_an_unreachable_micad_and_still_answers_200() {
    let (router, token) = failing_app(None).await;

    let response = bearer(&router, "GET", "/api/v1/health", &token).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a dead micad is reported in the body, never as a status code"
    );
    assert_api_headers(&response, "/api/v1/health");
    assert_eq!(response.headers().get(RETRY_AFTER), None);
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(body["apid"], json!("ok"));
    assert_eq!(body["micad"], json!("unreachable"));
    assert!(
        body["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the unreachable answer must say why: {body}"
    );
    assert_eq!(body["code"], json!("micad_unreachable"), "{body}");
    assert!(body.get("checkedAt").is_none(), "{body}");
}

// The failure codes on the health path: all three failure classes, and the ok
// answer that carries no code at all.
#[tokio::test]
pub(super) async fn the_health_route_classifies_every_failure_and_codes_none_of_the_successes() {
    // 1. The bounded call expired. Distinguishable from case 2 only by type.
    let (router, token) = timing_out_app().await;
    let response = bearer(&router, "GET", "/api/v1/health", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(body["micad"], json!("unreachable"));
    assert_eq!(body["code"], json!("micad_timeout"), "{body}");

    // 2. micad answered, with something that is not a count of seconds. Not
    // reported as health: `ok` has to mean the round trip produced a usable
    // answer.
    let (router, _fake, token) = health_app_with_token(json!("about ninety thousand"));
    let response = bearer(&router, "GET", "/api/v1/health", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(body["micad"], json!("unreachable"));
    assert_eq!(body["code"], json!("micad_bad_answer"), "{body}");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("about ninety thousand")),
        "the words stay in `detail`, which is what makes the code safe to close: {body}"
    );

    // 3. The healthy answer names no failure, so it carries no code: a member
    // present with a meaningless value is worse than an absent one.
    let (router, _fake, token) = health_app_with_token(json!(90_061));
    let response = bearer(&router, "GET", "/api/v1/health", &token).await;
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(body["micad"], json!("ok"));
    assert!(body.get("code").is_none(), "{body}");
    assert!(body.get("detail").is_none(), "{body}");
}

// The probe is a live bus call and not a flag: move the appliance's uptime and
// the next answer moves with it, in the same router and the same session.

// Authenticated like every other `/api/v1/` route, and its refusal is the error
// envelope rather than the gate's HTML redirect.
#[tokio::test]
pub(super) async fn health_without_a_session_is_the_envelope_and_not_a_redirect() {
    let (router, _) = health_app(90_061);

    let response = get(&router, "/api/v1/health", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers().get(LOCATION), None);
    assert_api_headers(&response, "/api/v1/health anonymous");
    assert_eq!(envelope(response).await["code"], "not_authenticated");
}

// `/healthz` is unchanged by any of this, and this test is the pin.
#[tokio::test]
pub(super) async fn healthz_is_untouched_by_the_health_route() {
    let (router, _) = health_app(90_061);

    let response = get(&router, "/healthz", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, CONTENT_TYPE),
        "text/plain; charset=utf-8"
    );
    assert_eq!(body_string(response).await, "ok");

    // Still `ok` with micad dead, which is the trap the health design
    // answers with a second route rather than by changing this one.
    let (failing, _) = failing_app(None).await;
    let response = get(&failing, "/healthz", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "ok");
}

// Every declared `/api/` route, on a method it does not serve: the error
// envelope, 405, and an `Allow` header naming what the route does serve.
//
// The expectation names the `Allow` value per route rather than deriving it,
// so a route that quietly gained or lost a method fails here.
#[tokio::test]
pub(super) async fn a_wrong_method_on_a_declared_api_route_answers_the_envelope() {
    let (router, fake) = health_app(90_061);
    fake.set_state_entry(
        "network",
        json!({ "wg0": { "kind": "wireguard", "publicKey": "k" } }),
    );
    let cookie = login(&router, "hunter2secret").await;

    for (method, path, allow) in [
        ("POST", "/api/versions", "GET,HEAD"),
        ("POST", "/api/v1/meta", "GET,HEAD"),
        ("DELETE", "/api/v1/health", "GET,HEAD"),
        ("POST", "/api/v1/settings/hostname", "GET,HEAD,PUT"),
        ("PUT", "/api/v1/state/uptime", "GET,HEAD"),
        ("GET", "/api/v1/actions/change-password", "POST"),
        ("GET", "/api/v1/actions/wireguard/wg0/rotate-key", "POST"),
        // The three action verbs. `GET` on each of them is the assertion that no
        // `GET` handler is declared: the HTML router refuses the same thing
        // deliberately so a browser prefetch, a crawler or a mis-clicked link
        // cannot power the appliance off, and `actions` is named `actions` so
        // no reader expects a `GET` to work there. Asserted here rather than in
        // a test of their own so the `Allow` value is checked by the same
        // per-route expectation every other declared route is checked by.
        ("GET", "/api/v1/actions/reboot", "POST"),
        ("GET", "/api/v1/actions/poweroff", "POST"),
        ("GET", "/api/v1/actions/transient-root-password", "POST"),
        ("PUT", "/api/v1/tokens", "GET,HEAD,POST"),
        ("GET", "/api/v1/tokens/deadbeef", "DELETE"),
        // The setup route. `GET` on it for the reason the three actions above
        // get one: it is the assertion that no `GET` handler is declared, made
        // by the same per-route expectation as every other declared route. On
        // a configured device this is a 405 and not the 409 a `POST` gets --
        // the router refuses the method before the handler sees the tree.
        ("GET", "/api/v1/setup", "POST"),
    ] {
        let response = request(&router, method, path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
        assert_eq!(header_value(&response, ALLOW), allow, "{method} {path}");
        assert_api_headers(&response, &format!("{method} {path}"));
        let error = envelope(response).await;
        assert_eq!(error["code"], "method_not_allowed", "{method} {path}");
        // apid, not micad: the router refused this before any bus call.
        assert_eq!(error["source"], "apid", "{method} {path}");
        assert!(
            error["message"].is_string(),
            "{method} {path}: the error envelope requires a message"
        );
        // A wrong method names no settings dot-path, so the optional member is
        // absent rather than empty.
        assert!(error.get("path").is_none(), "{method} {path}: {error}");
    }
}

// The 405 is the router's answer and not an authenticated one, which is what
// the shipped tree already did: `is_declared_api_route` tests the path and not
// the method, so the gate hands a wrong-method request off exactly as it hands
// off a right one. Recorded because it is a property, not an accident.
#[tokio::test]
pub(super) async fn the_405_envelope_does_not_depend_on_a_session() {
    let (router, _) = health_app(90_061);

    let response = request(&router, "POST", "/api/v1/meta", None, None).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(LOCATION), None);
    assert_eq!(header_value(&response, ALLOW), "GET,HEAD");
    assert_eq!(envelope(response).await["code"], "method_not_allowed");
}

// The asset router's 405 is outside `/api/` and is not unified with the one
// above: condition 2 gives it a bare body, its own
// `Allow: GET, HEAD` and no `Content-Type` at all, and the error envelope is a
// promise about `/api/v1/` routes only.

// The reserved subtree's own 404 is untouched by the 405: a path the API does
// not declare is still `not_found`, on every method, health-adjacent spellings
// included.
#[tokio::test]
pub(super) async fn the_api_fallback_404_survives_the_405() {
    let (router, _) = health_app(90_061);
    let cookie = login(&router, "hunter2secret").await;

    for (method, path) in [
        ("GET", "/api/v1/health/extra"),
        ("POST", "/api/v1/health/extra"),
        ("GET", "/api/v1/healthz"),
        ("DELETE", "/api/v1/nope"),
        ("POST", "/api/nope"),
    ] {
        let response = request(&router, method, path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(response.headers().get(ALLOW), None, "{method} {path}");
        assert_eq!(
            envelope(response).await["code"],
            "not_found",
            "{method} {path}"
        );
    }
}

// The document describes the route and its outcome: a
// client reading only `openapi.json` has to be able to learn both.

// The bearer token — the credential, the three `/api/v1/tokens` routes,
// and the bootstrap pane under the reserved prefix.
