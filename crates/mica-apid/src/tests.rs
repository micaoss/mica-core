//! Route-level tests driving the router directly with the fake settings
//! backend; no network or bus daemon involved.
//!
//! The exceptions are [`power_bus`] and [`settings_signal`], which drive the
//! real D-Bus client against a fake micad on a private bus, because what they
//! assert lives below the fake backend's trait.

mod broken_classes;
mod claim;
mod credential_rotation;
mod diagnostics;
mod power_bus;
mod provisioning_api;
mod reset;
mod settings_signal;
mod update_api;

use crate::auth;
use crate::routes::{AppState, app};
use crate::settings_api::{FakeSettings, InvalidTaskPayload, SettingsApi};
use crate::task_registry::TaskRecord;
use axum::Router;
use axum::body::Body;
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, LOCATION, RETRY_AFTER, SET_COOKIE,
};
use axum::http::{Request, Response, StatusCode};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

mod assets;
mod bluetooth;
mod containers;
mod errors;
mod health;
mod keys;
mod logs;
mod meta;
mod mqtt;
mod network_ifaces;
mod network_peers;
mod network_read;
mod network_routes;
mod password;
mod power;
mod resources;
mod schemas;
mod session;
mod settings_write;
mod setup;
mod tasks;
mod tokens;
mod ui_bundles;
mod ui_console;
mod web;
mod wifi_ap;
mod wifi_networks;
mod wifi_station;
mod wireguard;
use assets::*;
use errors::*;
use keys::*;
use meta::*;
use mqtt::*;
use network_ifaces::*;
use password::*;
use resources::*;
use settings_write::*;
use tokens::*;
use ui_console::*;
use wifi_networks::*;
use wifi_station::*;
use wireguard::*;

const SIGNING_KEY: [u8; 32] = [7u8; 32];

fn test_app(tree: serde_json::Value) -> (Router, Arc<FakeSettings>) {
    let fake = Arc::new(FakeSettings::new(tree));
    let state = AppState::new(fake.clone(), SIGNING_KEY).with_builtin_ui(console());
    (app(state), fake)
}

/// The built-in console these tests serve: a tree shaped like the package's,
/// written once for the whole process because apid reads a file when it is
/// asked for it, so the tree has to outlive every router.
fn console() -> &'static Path {
    static CONSOLE: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    CONSOLE
        .get_or_init(|| {
            let dir = TempDir::new().unwrap();
            for (path, body) in CONSOLE_FILES {
                let file = dir.path().join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, body).unwrap();
            }
            dir
        })
        .path()
}

/// The fixture console: an entry document, a hashed script and lazy chunk
/// under `assets/`, and a file outside it.
const CONSOLE_FILES: [(&str, &str); 4] = [
    (
        "index.html",
        "<!doctype html><title>mica console</title><script type=module src=/_ui/assets/index-a1b2.js></script>",
    ),
    ("assets/index-a1b2.js", "console.log('built-in')"),
    ("assets/zh-cn-c3d4.js", "export default {}"),
    ("favicon.svg", "<svg xmlns='http://www.w3.org/2000/svg'/>"),
];

/// The console's script, which a custom bundle tries to shadow.
const CONSOLE_JS: &str = "assets/index-a1b2.js";

fn unconfigured_tree() -> serde_json::Value {
    json!({ "hostname": "mica", "network": {}, "access": {} })
}

fn configured_tree(password: &str) -> serde_json::Value {
    let hash = auth::hash_password(password).unwrap();
    json!({
        "hostname": "mica",
        "network": {},
        "access": { "webAdmin": { "password_hash": hash } },
    })
}

// Log in against a configured tree and return the session cookie value.
async fn login(router: &Router, password: &str) -> String {
    let response = json_request(
        router,
        "POST",
        "/api/v1/session",
        json!({ "password": password }),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    session_cookie_value(&response)
}

async fn body_string(response: Response<axum::body::Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// A bounded call is not an ordinary connectivity failure: micad may still be
// applying a write after apid gives the request back, so the response must
// say "not confirmed" (504) rather than "unreachable" (503).
#[tokio::test]
async fn a_micad_call_timeout_has_its_own_api_classification() {
    let err = anyhow::Error::new(crate::bus_client::MicadCallTimeout::new(
        "SetSettings",
        std::time::Duration::from_secs(5),
    ));

    let response = crate::routes::bus_api_error(&err, Some("hostname"));
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(response.headers().get(RETRY_AFTER).is_none());
    let body: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("JSON envelope");
    assert_eq!(body["error"]["code"], "micad_timeout");
    assert_eq!(body["error"]["path"], "hostname");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("may still be running")),
        "the timeout must not claim the operation failed: {body}"
    );
}

// A task payload that reached apid but does not match the bus contract is a
// daemon failure, not a connectivity outage, and must match OpenAPI's 500.
#[tokio::test]
async fn an_invalid_task_payload_is_a_micad_failure() {
    let parse_error =
        serde_json::from_str::<TaskRecord>("{}").expect_err("an empty object is not a task record");
    let err = anyhow::Error::new(InvalidTaskPayload(parse_error));

    let response = crate::routes::bus_api_error(&err, None);
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers().get(RETRY_AFTER).is_none());
    let body: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("JSON envelope");
    assert_eq!(body["error"]["code"], "micad_failed");
    assert_eq!(body["error"]["source"], "micad");
}

async fn send(router: &Router, request: Request<Body>) -> Response<axum::body::Body> {
    router.clone().oneshot(request).await.unwrap()
}

async fn get(router: &Router, path: &str, cookie: Option<&str>) -> Response<axum::body::Body> {
    let mut builder = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, format!("apid_session={cookie}"));
    }
    send(router, builder.body(Body::empty()).unwrap()).await
}

async fn post_form(
    router: &Router,
    path: &str,
    body: &str,
    cookie: Option<&str>,
) -> Response<axum::body::Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, format!("apid_session={cookie}"));
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

async fn json_request(
    router: &Router,
    method: &str,
    path: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> Response<axum::body::Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, format!("apid_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

async fn zip_request(
    router: &Router,
    path: &str,
    bytes: Vec<u8>,
    cookie: &str,
    csrf: Option<&str>,
) -> Response<axum::body::Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, "application/zip")
        .header(COOKIE, format!("apid_session={cookie}"));
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    send(router, builder.body(Body::from(bytes)).unwrap()).await
}

/// A bearer-authenticated POST of raw bytes under a chosen content type: the
/// shape an archive upload has.
async fn bearer_bytes(
    router: &Router,
    path: &str,
    bytes: Vec<u8>,
    content_type: &str,
    token: &str,
) -> Response<axum::body::Body> {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, content_type)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(bytes))
        .unwrap();
    send(router, request).await
}

fn location(response: &Response<axum::body::Body>) -> &str {
    response.headers().get(LOCATION).unwrap().to_str().unwrap()
}

// The `apid_session=<value>` part of the `Set-Cookie` response header.
fn session_cookie_value(response: &Response<axum::body::Body>) -> String {
    let header = response
        .headers()
        .get(SET_COOKIE)
        .expect("Set-Cookie header")
        .to_str()
        .unwrap();
    let (pair, attrs) = header.split_once(';').unwrap();
    for attr in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(attrs.contains(attr), "cookie should carry {attr}: {header}");
    }
    pair.strip_prefix("apid_session=").unwrap().to_string()
}
