//! Versions, metadata, the OpenAPI document and the audit trail.

use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Response, StatusCode};
use serde_json::json;
use std::path::Path;

use super::*;

// The exact document the discovery table gives for the served set.
pub(super) const VERSIONS_BODY: &str = r#"{"versions":["v1"],"current":"v1"}"#;

// The exact document the discovery table gives for `/api/v1/meta`.
pub(super) fn meta_body() -> String {
    format!(
        r#"{{"api":"v1","settingsSchemaVersion":{},"daemon":"apid","features":["wifi","bluetooth","ssh","containers","mqtt"]}}"#,
        micad_settings::STATE_SCHEMA_VERSION
    )
}

// Both headers on every `/api/` response, successes included.
pub(super) fn assert_api_headers(response: &Response<axum::body::Body>, context: &str) {
    assert_eq!(
        header_value(response, CONTENT_TYPE),
        "application/json",
        "{context}"
    );
    assert_eq!(
        header_value(response, CACHE_CONTROL),
        "no-store",
        "{context}"
    );
}

// The `error` object of an error envelope.
pub(super) async fn envelope(response: Response<axum::body::Body>) -> serde_json::Value {
    let body = body_string(response).await;
    let parsed: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("error envelope, got: {body}"));
    parsed["error"].clone()
}

// Unauthenticated, and the answer is the table's document exactly.
#[tokio::test]
pub(super) async fn api_versions_answers_the_served_set_without_a_session() {
    let (router, _) = test_app(configured_tree("hunter2secret"));

    let response = get(&router, "/api/versions", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/versions");
    assert_eq!(body_string(response).await, VERSIONS_BODY);
}

// The first of the two reasons the endpoint is unauthenticated: a
// factory-fresh device has no `access.webAdmin`, so the gate is in setup mode
// and sends everything else to `/setup`.

// The hand-off is above the gate's `GetSettings("access")` call, so the
// question "which versions does this device serve?" is still answerable when
// micad is not answering.

// The second discovery endpoint, answered for a valid session.
#[tokio::test]
pub(super) async fn api_v1_meta_answers_for_a_session() {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    let response = bearer(&router, "GET", "/api/v1/meta", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/meta");
    assert_eq!(body_string(response).await, meta_body());
}

// The trap, refused: a client that follows the gate's redirect lands on
// `GET /login`, which answers **200 with HTML**, so a script reads the whole
// exchange as success. The answer is the error envelope with the status that
// matches it.
#[tokio::test]
pub(super) async fn api_v1_meta_without_a_session_is_401_and_the_envelope() {
    let (router, _) = test_app(configured_tree("hunter2secret"));

    let response = get(&router, "/api/v1/meta", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_api_headers(&response, "/api/v1/meta");
    let error = envelope(response).await;
    assert_eq!(error["code"], "not_authenticated");
    assert_eq!(error["source"], "apid");
    assert!(
        error["message"].is_string(),
        "the error envelope requires a message"
    );
    // `path` is the settings dot-path at fault, and a request
    // that failed to authenticate names none.
    assert_eq!(error.get("path"), None);
}

// Setup mode is the branch a path-prefix implementation breaks: no session
// can exist there, and the gate sends everything it still owns to `/setup`.
#[tokio::test]
pub(super) async fn api_v1_meta_is_401_in_setup_mode_too() {
    let (router, _) = test_app(unconfigured_tree());

    let response = get(&router, "/api/v1/meta", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_api_headers(&response, "/api/v1/meta");
    assert_eq!(envelope(response).await["code"], "not_authenticated");
}

// The declared paths are the only ones that changed. Every other path under
// `/api` has **one** answer — the subtree's own 404, byte for byte — with a
// session, without one, and in setup mode alike.
#[tokio::test]
pub(super) async fn every_other_api_path_has_one_answer_in_every_mode() {
    // `/api/v1/ssh/authorized-keys` is a served collection, not on this list;
    // the test that holds its answers is
    // `the_ssh_key_collection_lists_adds_and_removes`.
    const UNDECLARED: [&str; 4] = [
        "/api/v1/settings",
        "/api/v1/settings/",
        "/api/v1/state",
        "/api/v1/state/",
    ];

    let (router, _) = test_app(configured_tree("hunter2secret"));
    let cookie = login(&router, "hunter2secret").await;
    let (fresh, _) = test_app(unconfigured_tree());

    for path in UNDECLARED {
        let expected = json!({
            "error": {
                "code": "not_found",
                "message": format!("no API route at {path}"),
                "source": "apid",
            }
        })
        .to_string();

        // With a session, without one, and on a device that has no admin
        // password at all: the reserved subtree's own envelope, byte for byte,
        // three times. The router is the same one in the first two cases and a
        // freshly built one in setup mode, which is the case a path-prefix
        // gate would break.
        for (mode, response) in [
            ("with a session", get(&router, path, Some(&cookie)).await),
            ("without one", get(&router, path, None).await),
            ("in setup mode", get(&fresh, path, None).await),
        ] {
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path} {mode}");
            assert_api_headers(&response, path);
            assert_eq!(body_string(response).await, expected, "{path} {mode}");
        }
    }
}

// `micad/apid/openapi.json` is the bytes `apid --openapi` prints.
//
// A local `cargo test` failure and not only a CI one: whoever changed a route
// is the person holding the command that regenerates the file.
#[test]
pub(super) fn the_committed_openapi_document_is_the_generated_one() {
    assert_eq!(
        include_str!("../../openapi.json"),
        crate::openapi::document_json().unwrap()
    );
}

#[test]
pub(super) fn the_openapi_document_covers_browser_ui_and_live_network_routes() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let session = &document["paths"]["/api/v1/session"];
    for method in ["get", "post", "delete"] {
        assert!(
            session[method].is_object(),
            "missing session {method}: {session}"
        );
    }
    assert!(document["paths"]["/api/v1/ui"]["get"].is_object());
    assert!(document["paths"]["/api/v1/ui/active"]["put"].is_object());
    assert!(document["paths"]["/api/v1/ui/active"]["delete"].is_object());
    assert!(
        document["components"]["schemas"]["UiStatus"]["properties"]["availableCustom"].is_object()
    );
    assert_eq!(
        document["components"]["schemas"]["CustomUiUnavailableReason"]["enum"],
        json!([
            "missingActivationRecord",
            "unsafeTree",
            "indexUnavailable",
            "manifestInvalid",
            "digestMismatch",
            "incompatible",
        ])
    );

    let network = &document["paths"]["/api/v1/network"];
    assert!(
        network["get"].is_object(),
        "missing live network read: {network}"
    );
    assert!(
        network["put"].is_object(),
        "missing network write: {network}"
    );
    assert!(
        document["components"]["schemas"]["NetworkOverview"]["properties"]["observed"].is_object()
    );
    assert!(
        document["components"]["schemas"]["SetupResult"]["properties"]["csrfToken"].is_object()
    );
    // Setup mints no API token, so its response carries no secret at all.
    assert!(document["components"]["schemas"]["SetupResult"]["properties"]["token"].is_null());
}

// The document describes the served surface, the outcome included: a
// client that reads only `openapi.json` has to be able to learn that
// `/api/v1/meta` can answer 401.
#[test]
pub(super) fn the_openapi_document_covers_the_declared_routes() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    assert!(
        document["paths"]["/api/versions"]["get"]["responses"]["200"].is_object(),
        "{document}"
    );
    let meta = &document["paths"]["/api/v1/meta"]["get"]["responses"];
    assert!(meta["200"].is_object(), "{meta}");
    assert!(meta["401"].is_object(), "{meta}");
}

// Rules 2 and 3: a declared route wins structurally, and the bundle
// files of the same name are never consulted.

// Every line of the audit log under `dir`, parsed, oldest first.
pub(super) fn audit_lines(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("audit.log"))
        .expect("the audit log exists")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every audit line parses as JSON"))
        .collect()
}

// The `(event, outcome)` pairs of `lines`, for order-sensitive assertions.
pub(super) fn audit_events(lines: &[serde_json::Value]) -> Vec<(String, String)> {
    lines
        .iter()
        .map(|line| {
            (
                line["event"].as_str().expect("event").to_string(),
                line["outcome"].as_str().expect("outcome").to_string(),
            )
        })
        .collect()
}

// The MQTT pane
