//! The asset router: the API reservation, methods, fallbacks and headers.

use crate::assets::serve;
use crate::bundle::Store;
use crate::routes::{AppState, app};
use crate::settings_api::FakeSettings;
use axum::Router;
use axum::body::Body;
use axum::http::header::{ACCEPT, ALLOW, CACHE_CONTROL, CONTENT_TYPE, COOKIE};
use axum::http::{HeaderName, Request, Response, StatusCode};
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

use super::*;

#[tokio::test]
pub(super) async fn html_form_mutations_are_not_routes() {
    let empty = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_bundle_root(empty.path()));
    for path in [
        "/containers/enable",
        "/mqtt/enable",
        "/setup",
        "/ssh/enable",
        "/ssh/password",
        "/ssh/keys/add",
        "/ssh/keys/remove",
        "/network",
        "/network/peers/add",
        "/network/peers/remove",
        "/hostname",
        "/password",
        "/power/reboot",
        "/power/poweroff",
        "/login",
        "/logout",
        "/builtin/deactivate",
        "/builtin/tokens",
        "/builtin/tokens/revoke",
    ] {
        let response = post_form(&router, path, "enabled=on", None).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }
    assert!(fake.set_paths().is_empty());
}

// The root asset router without the structural `/api` declaration.
//
// The shared path layer still refuses a literal or encoded reserved first
// segment. This is intentional defence in depth for ambiguous spellings that
// Axum did not classify, not a route to API content.
pub(super) fn asset_router_without_the_api_reservation(bundle_root: &Path) -> Router {
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    let state = AppState::new(fake, SIGNING_KEY).with_bundle_root(bundle_root);
    Router::new().fallback(serve::fallback).with_state(state)
}

pub(super) async fn request(
    router: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    accept: Option<&str>,
) -> Response<axum::body::Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, format!("apid_session={cookie}"));
    }
    if let Some(accept) = accept {
        builder = builder.header(ACCEPT, accept);
    }
    send(router, builder.body(Body::empty()).unwrap()).await
}

pub(super) fn header_value(response: &Response<axum::body::Body>, name: HeaderName) -> String {
    response
        .headers()
        .get(&name)
        .unwrap_or_else(|| panic!("response carries {name}"))
        .to_str()
        .unwrap()
        .to_string()
}

// Rule 1, stated the way condition 1 needs it: a bundle that
// **actually contains** files under `api/` cannot serve one.
#[tokio::test]
pub(super) async fn a_bundle_cannot_shadow_the_reserved_api_subtree() {
    const VERSIONS_BYTES: &str = "BUNDLE-SHADOWS-API-VERSIONS";
    const SETTINGS_BYTES: &str = "BUNDLE-SHADOWS-API-V1-SETTINGS";

    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("decoy.txt", "the bundle is reachable"),
        ("api/versions", VERSIONS_BYTES),
        ("api/v1/settings", SETTINGS_BYTES),
    ]);

    // The files are in the installed tree, not merely in the staged one.
    let listed = installed_files(bundle.path());
    assert!(
        listed.contains(&"api/versions".to_string())
            && listed.contains(&"api/v1/settings".to_string()),
        "the installed bundle must actually contain the shadowing files: {listed:?}"
    );

    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    // The bundle is reachable through this very router, so a 404 under `/api/`
    // cannot be explained by the bundle not being served.
    let response = request(&router, "GET", "/decoy.txt", Some(&cookie), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "the bundle is reachable");

    // The declared route answers with its own document, not with the file the
    // bundle put in its way.
    let response = request(&router, "GET", "/api/versions", Some(&cookie), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        !body.contains(VERSIONS_BYTES),
        "/api/versions answered with the bundle's own bytes: {body}"
    );
    assert_eq!(body, VERSIONS_BODY);

    for (path, bytes) in [("/api/v1/settings", SETTINGS_BYTES)] {
        let response = request(&router, "GET", path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        let status = response.status();
        let content_type = header_value(&response, CONTENT_TYPE);
        let cache_control = header_value(&response, CACHE_CONTROL);
        let body = body_string(response).await;

        // Asserted first, and on the body rather than on the status, so that
        // deleting the reservation fails this test **with the bundle's own
        // bytes printed** rather than with a bare `200 != 404`. The guard is
        // then distinguishable from its absence by reading the failure.
        assert!(
            !body.contains(bytes),
            "{path}: the reserved subtree answered with the bundle's own bytes: {body}"
        );
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(content_type, "application/json", "{path}");
        assert_eq!(cache_control, "no-store", "{path}");
        let envelope: serde_json::Value = serde_json::from_str(&body).expect("error envelope");
        assert_eq!(envelope["error"]["code"], "not_found", "{path}");
        assert_eq!(envelope["error"]["source"], "apid", "{path}");
        assert!(
            envelope["error"]["message"].is_string(),
            "{path}: the error envelope requires a message"
        );
    }
}

// Reserved ownership is based on one canonical URL spelling. Repeated
// leading separators and percent-encoded reserved names must be rejected,
// never decoded by the root asset resolver into a second spelling of `/api`
// or `/_ui`. Prefix lookalikes and `/ui` remain ordinary custom-UI paths.
#[tokio::test]
pub(super) async fn ambiguous_reserved_prefixes_cannot_cross_asset_roots() {
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("api/versions", "CUSTOM-API-ALIAS"),
        ("_ui/assets/app.js", "CUSTOM-UI-ALIAS"),
        ("ui/asset.js", "CUSTOM-UI"),
        ("apiary/asset.js", "CUSTOM-APIARY"),
        ("uikit/asset.js", "CUSTOM-UIKIT"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    for path in [
        "//api/versions",
        "/%61pi/versions",
        "/%5fui/assets/app.js",
        "/_ui/%252e%252e",
        "/_ui/%2e%2e/api",
    ] {
        let response = request(&router, "GET", path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{path} must terminate in its originally selected ownership domain"
        );
        let body = body_string(response).await;
        assert!(!body.contains("CUSTOM-"), "{path} crossed into custom UI");
        assert!(
            !body.contains("<title>mica console</title>"),
            "{path} used a guarded miss as built-in SPA navigation"
        );
    }

    for (path, expected) in [
        ("/apiary/asset.js", "CUSTOM-APIARY"),
        ("/ui/asset.js", "CUSTOM-UI"),
        ("/uikit/asset.js", "CUSTOM-UIKIT"),
    ] {
        let response = request(&router, "GET", path, Some(&cookie), None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(body_string(response).await, expected, "{path}");
    }
}

// Even if a caller constructs the root asset router without the structural
// `/api` reservation, the shared logical-path guard will not expose a bundle's
// reserved first segment.
#[tokio::test]
pub(super) async fn root_asset_resolver_fails_closed_without_the_api_router() {
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("api/versions", "BUNDLE-SHADOWS-API-VERSIONS"),
    ]);
    let unreserved = asset_router_without_the_api_reservation(bundle.path());

    let response = request(&unreserved, "GET", "/api/versions", None, None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_string(response).await, "");
}

// The reservation covers the subtree and every method, for every path the
// API does not declare. The declared paths are asserted separately, below.
#[tokio::test]
pub(super) async fn the_api_reservation_answers_every_shape_with_the_envelope() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom</title>")]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    for (method, path) in [
        ("GET", "/api"),
        ("GET", "/api/"),
        ("GET", "/api/v1"),
        ("GET", "/api/versions/extra"),
        ("GET", "/api/v1/settings"),
        // `/api/v1/actions/reboot` was here until it was declared, and
        // its prefix and trailing-slash spelling take its place for the reason
        // the WiFi collection's did: neither is a route this router serves, so
        // both must still reach the reservation rather than the three action
        // routes beside them.
        ("GET", "/api/v1/actions"),
        ("GET", "/api/v1/actions/"),
        // `/api/v1/wifi/client/networks` was here until it was declared, and
        // `/api/v1/wifi/client` until the station role was. The spellings that
        // remain are the trailing-slash ones: neither is a route this router
        // serves, so both must still reach the reservation rather than the
        // resources beside them.
        ("GET", "/api/v1/wifi/"),
        ("GET", "/api/v1/wifi/client/networks/"),
        ("POST", "/api/v1/settings"),
    ] {
        let response = request(&router, method, path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {path} must be the reserved subtree's own 404"
        );
        assert_eq!(
            header_value(&response, CONTENT_TYPE),
            "application/json",
            "{method} {path}"
        );
        let envelope: serde_json::Value =
            serde_json::from_str(&body_string(response).await).expect("error envelope");
        assert_eq!(envelope["error"]["code"], "not_found", "{method} {path}");
    }
}

// The two discovery endpoints, and what the rest of the reserved subtree
// answers beside its two declared paths.

// Condition 2. Anything that is not `GET` or `HEAD` and reaches the
// asset router is a client error, and it is never HTML.
#[tokio::test]
pub(super) async fn a_write_method_reaching_the_asset_router_is_405_and_never_html() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom</title>")]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let response = request(
            &router,
            method,
            "/settings/network",
            Some(&cookie),
            Some(BROWSER_ACCEPT),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} /settings/network"
        );
        assert_eq!(header_value(&response, ALLOW), "GET, HEAD", "{method}");
        assert!(response.headers().get(CONTENT_TYPE).is_none(), "{method}");
        assert_eq!(body_string(response).await, "", "{method}");
    }
}

// Condition 3. This is the condition that separates a navigation from a
// data call when both are `GET`, and it is the whole of the stated
// property: a request a developer expected to be JSON never comes back as
// HTML with a 200.
#[tokio::test]
pub(super) async fn a_json_client_never_gets_the_spa_fallback() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom</title>")]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    // Three shapes of data call, and none of them may come back as HTML: the
    // explicit one, the `*/*` a `fetch()` sends when it sets no
    // `Accept`, and no header at all.
    for accept in [Some("application/json"), Some("*/*"), None] {
        let response = request(&router, "GET", "/settings/network", Some(&cookie), accept).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{accept:?}");
        assert_eq!(body_string(response).await, "", "{accept:?}");
    }

    // The same path, asked for as a navigation.
    for accept in [BROWSER_ACCEPT, "text/html"] {
        let response = request(
            &router,
            "GET",
            "/settings/network",
            Some(&cookie),
            Some(accept),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{accept}");
        assert_eq!(
            body_string(response).await,
            "<!doctype html><title>custom</title>",
            "{accept}"
        );
    }
}

// Condition 4, implemented as a heuristic: a final segment
// with a `.` is a filename, and a miss on a filename is a 404 with an empty
// body even for a browser navigation.
#[tokio::test]
pub(super) async fn a_dotted_final_segment_misses_with_an_empty_body() {
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("assets/app.a1b2c3.js", "//real"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    let response = request(
        &router,
        "GET",
        "/assets/app.deadbeef.js",
        Some(&cookie),
        Some(BROWSER_ACCEPT),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_string(response).await, "");

    // A path with no dot in its final segment is a client-side route.
    let response = request(
        &router,
        "GET",
        "/settings/network",
        Some(&cookie),
        Some(BROWSER_ACCEPT),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // And the file that does exist is served, so the 404 above is a miss and
    // not the extension being refused.
    let response = request(
        &router,
        "GET",
        "/assets/app.a1b2c3.js",
        Some(&cookie),
        Some(BROWSER_ACCEPT),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "//real");
}

// Condition 5, and classes 1 and 2: with no readable index the
// answer is the **built-in UI**, not a 404 and not a 500.

// The `/` exception, both branches.

// The headers as applied: `nosniff` on every asset response, the content type from
// the allowlist, and the cache class per the table — including the
// manifest's opt-in immutable directory.
#[tokio::test]
pub(super) async fn every_asset_response_carries_nosniff_and_its_cache_class() {
    let manifest = r#"{"name":"custom","version":"1.0",
        "immutableDir":"assets","apiVersions":["v1"]}"#;
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("mica-ui.json", manifest),
        ("assets/app.a1b2c3.js", "//real"),
        ("assets/logo.svg", "<svg/>"),
        ("robots.txt", "User-agent: *"),
        ("data.bin", "\u{0}\u{1}"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    for (path, content_type, cache_control) in [
        ("/index.html", "text/html; charset=utf-8", "no-store"),
        (
            "/assets/app.a1b2c3.js",
            "text/javascript; charset=utf-8",
            "public, max-age=31536000, immutable",
        ),
        (
            "/assets/logo.svg",
            "image/svg+xml",
            "public, max-age=31536000, immutable",
        ),
        ("/robots.txt", "text/plain; charset=utf-8", "no-cache"),
        ("/data.bin", "application/octet-stream", "no-cache"),
    ] {
        let response = request(&router, "GET", path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            header_value(&response, CONTENT_TYPE),
            content_type,
            "{path}"
        );
        assert_eq!(
            header_value(&response, CACHE_CONTROL),
            cache_control,
            "{path}"
        );
        assert_eq!(
            header_value(&response, HeaderName::from_static("x-content-type-options")),
            "nosniff",
            "{path}"
        );
    }

    // The SPA fallback is an HTML document and is `no-store` with it, explicitly, because a cached index makes a new bundle
    // invisible however correctly its assets are named.
    let response = request(
        &router,
        "GET",
        "/settings/network",
        Some(&cookie),
        Some(BROWSER_ACCEPT),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_value(&response, CACHE_CONTROL), "no-store");
    assert_eq!(
        header_value(&response, HeaderName::from_static("x-content-type-options")),
        "nosniff"
    );

    // A refusal is an asset response too.
    let response = request(
        &router,
        "GET",
        "/assets/missing.js",
        Some(&cookie),
        Some(BROWSER_ACCEPT),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        header_value(&response, HeaderName::from_static("x-content-type-options")),
        "nosniff"
    );
}

// The suite, at the router rather than at `assets::path`: a hostile request
// is a 404 and is never answered by the fallback, even though every one of
// these satisfies the own five conditions.
#[tokio::test]
pub(super) async fn a_hostile_path_is_404_and_never_the_spa_fallback() {
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("etc/passwd", "decoy"),
    ]);
    let store = Store::new(bundle.path());
    std::os::unix::fs::symlink("/etc/passwd", store.bundle_dir(1).join("leak")).unwrap();

    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    for path in [
        "/../../etc/passwd",
        "/%2e%2e%2fetc%2fpasswd",
        "/%252e%252e%2fetc%2fpasswd",
        "/index%00",
        "/leak",
    ] {
        let response = request(&router, "GET", path, Some(&cookie), Some(BROWSER_ACCEPT)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let body = body_string(response).await;
        assert_eq!(body, "", "{path} must have an empty body");
        assert!(!body.contains("root:"), "{path} read the real /etc/passwd");
    }
}

// `HEAD` is condition 2's other admitted method, and it answers with the
// headers its `GET` would carry.
#[tokio::test]
pub(super) async fn head_is_admitted_and_carries_the_same_headers_as_get() {
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom</title>"),
        ("assets/app.a1b2c3.js", "//real"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let cookie = login(&router, "hunter2secret").await;

    let head = request(
        &router,
        "HEAD",
        "/assets/app.a1b2c3.js",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        header_value(&head, CONTENT_TYPE),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(header_value(&head, CACHE_CONTROL), "no-cache");
    assert_eq!(
        header_value(&head, HeaderName::from_static("x-content-type-options")),
        "nosniff"
    );
}
