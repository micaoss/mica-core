//! The first-boot setup route.

use crate::routes::{AppState, app};
use crate::settings_api::{FakeSettings, SettingsApi};
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::{Request, StatusCode};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

use super::*;

pub(super) const SETUP_PATH: &str = "/api/v1/setup";
// A body that configures everything the route accepts.
pub(super) fn full_setup_body() -> String {
    json!({
        "password": "first-boot-pw",
        "hostname": "appliance",
        "network": { "eth0": { "kind": "physical", "dhcp": true } },
    })
    .to_string()
}

// The happy path: one unauthenticated call configures the device and hands
// back a credential that works.

// The write order, asserted as an order and not as a set.

// **One partial failure**, driven through both
// surfaces, with opposite outcomes.

// The setup acceptance criterion, on the rule the
// route cannot reach any other way.

// A relational rule that is legal only because of an entry the device already
// has: the candidate tree is the stored map plus the submission, not the
// submission alone.
#[tokio::test]
pub(super) async fn the_setup_network_tree_is_merged_with_the_stored_one_before_it_is_judged() {
    let (router, fake) = test_app(json!({
        "hostname": "mica",
        "network": { "eth1": { "kind": "physical", "dhcp": false } },
        "access": {},
    }));

    let body = json!({
        "password": "first-boot-pw",
        "network": { "br0": { "kind": "bridge", "dhcp": false, "bridge": { "ports": ["eth1"] } } },
    })
    .to_string();
    let response = post_json(&router, SETUP_PATH, &body, None).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Both entries are there: the submitted one was added and the stored one
    // was not dropped by the whole-map write.
    let network = fake.get_settings("network").await.unwrap();
    let mut names: Vec<&str> = network
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["br0", "eth1"], "{network}");
}

// 409 on the form path's own condition, and both surfaces are asserted on one
// tree so neither can drift into answering about a different one.

// Every validation failure, each with nothing written.

// A rejected password is never echoed, in the body or the headers.
//
// The same property the transient-password route has, and for the same
// reason: a refusal that repeats the secret puts it in every proxy log
// between the caller and the device.
#[tokio::test]
pub(super) async fn a_rejected_setup_password_is_never_echoed() {
    let (router, _) = test_app(unconfigured_tree());
    let response = post_json(
        &router,
        SETUP_PATH,
        &json!({ "password": "sh0rt!" }).to_string(),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let headers = format!("{:?}", response.headers());
    let body = body_string(response).await;
    for haystack in [&headers, &body] {
        assert!(!haystack.contains("sh0rt!"), "{haystack}");
        // Not a fragment of it either.
        assert!(!haystack.contains("sh0rt"), "{haystack}");
    }
}

// A body that is not JSON at all is 400, and it names no dot-path.
//
// The `path` is the settings dot-path at fault. This route writes three
// subtrees, and a body that never parsed is not about any of them, so the
// member is absent rather than naming one arbitrarily.

// The route takes no credential, and it is the only one under the prefix that
// does not.
#[tokio::test]
pub(super) async fn the_setup_route_is_the_one_api_route_that_takes_no_credential() {
    let (router, _) = test_app(unconfigured_tree());

    // Every other write route under the prefix, unauthenticated, in setup
    // mode: the 401 envelope and never a redirect.
    for (method, path, body) in [
        ("PUT", "/api/v1/settings/hostname", "\"appliance\""),
        ("POST", "/api/v1/actions/reboot", ""),
        ("POST", "/api/v1/tokens", "{\"name\":\"x\"}"),
        ("PUT", "/api/v1/network", "{}"),
        ("PUT", "/api/v1/mqtt", "{}"),
        ("POST", "/api/v1/ssh/authorized-keys", "{}"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = send(&router, request).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        assert_eq!(
            response.headers().get(LOCATION),
            None,
            "{method} {path} answered a redirect, which a script reads as success"
        );
        assert_eq!(
            envelope(response).await["code"],
            "not_authenticated",
            "{method} {path}"
        );
    }

    // And the setup route, with nothing at all: no cookie, no bearer.
    let response = post_json(&router, SETUP_PATH, &full_setup_body(), None).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

// The route records the CLAIM, and the password reaches no line of the trail.
#[tokio::test]
pub(super) async fn the_api_setup_route_records_the_claim_and_no_credential() {
    let dir = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(unconfigured_tree()));
    let router = app(AppState::new(fake, SIGNING_KEY).with_persistence(dir.path()));

    let response = post_json(&router, SETUP_PATH, &full_setup_body(), None).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&response);
    // The body carries the session's CSRF token and no credential of any
    // other kind: setup mints no API token.
    let body: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("setup JSON");
    assert!(body["csrfToken"].as_str().is_some(), "{body}");
    assert!(body.get("token").is_none(), "{body}");

    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("claim".to_string(), "completed".to_string())]
    );
    let raw = std::fs::read_to_string(dir.path().join("audit.log")).unwrap();
    for secret in ["first-boot-pw", cookie.as_str()] {
        assert!(!raw.contains(secret), "the trail must not carry {secret}");
    }
}
