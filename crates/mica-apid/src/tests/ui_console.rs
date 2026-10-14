//! The built-in console and the product's features.

use crate::bundle::Store;
use crate::routes::{AppState, app};
use crate::settings_api::FakeSettings;
use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderName, StatusCode};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

use super::*;

// What a browser sends on a navigation.
pub(super) const BROWSER_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

// The served set. Passed in because this phase does not define one
// and must not invent it; a bundle declaring `v1` intersects it.
pub(super) const SERVED: &[&str] = &["v1"];

// Stage `files` as generation 1 and activate it, returning the store's root.
pub(super) fn install_bundle(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("temp bundle store");
    let store = Store::new(dir.path());
    let staging = store.staging_dir(1);
    for (relative, contents) in files {
        let path = staging.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create staged parent");
        std::fs::write(path, contents).expect("write staged file");
    }
    store.activate(1, SERVED).expect("activate the staged tree");
    dir
}

// Every regular file in the installed tree, relative to the bundle root,
// sorted.
pub(super) fn installed_files(root: &Path) -> Vec<String> {
    let store = Store::new(root);
    let generation = store
        .active_generation()
        .expect("read current")
        .expect("a bundle is active");
    let bundle = store.bundle_dir(generation);
    let mut found = Vec::new();
    let mut stack = vec![bundle.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read installed dir") {
            let entry = entry.expect("installed dir entry");
            if entry.file_type().expect("entry type").is_dir() {
                stack.push(entry.path());
            } else {
                found.push(
                    entry
                        .path()
                        .strip_prefix(&bundle)
                        .expect("inside the bundle")
                        .display()
                        .to_string(),
                );
            }
        }
    }
    found.sort();
    found
}

// The router as shipped, with the bundle store rooted at `bundle_root`.
pub(super) fn test_app_serving(tree: serde_json::Value, bundle_root: &Path) -> Router {
    let fake = Arc::new(FakeSettings::new(tree));
    // A fixed uptime, so the status pane renders its uptime line (which
    // `without_the_uptime_line` requires) from the fake like everything else.
    fake.set_state_entry("uptime", json!(90_061));
    app(AppState::new(fake, SIGNING_KEY)
        .with_bundle_root(bundle_root)
        .with_builtin_ui(console()))
}

#[tokio::test]
pub(super) async fn ui_boundary_selects_custom_at_root_and_always_reserves_builtin_ui() {
    let empty = TempDir::new().unwrap();
    let router = test_app_serving(configured_tree("hunter2secret"), empty.path());
    let root = get(&router, "/", None).await;
    assert_eq!(root.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&root), "/_ui/");
    for path in ["/_ui", "/_ui/"] {
        let builtin = get(&router, path, None).await;
        assert_eq!(builtin.status(), StatusCode::OK, "{path}");
        assert_eq!(
            builtin.headers().get(CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert!(body_string(builtin).await.contains("/_ui/assets/"));
    }
    let old_builtin = request(&router, "GET", "/ui", None, Some(BROWSER_ACCEPT)).await;
    assert_eq!(old_builtin.status(), StatusCode::NOT_FOUND);

    let builtin_js = CONSOLE_JS;
    let custom_shadow = format!("_ui/{builtin_js}");
    let builtin_url = format!("/_ui/{builtin_js}");

    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom-root</title>"),
        (&custom_shadow, "CUSTOM-SHADOW"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
    let root = get(&router, "/", None).await;
    assert_eq!(root.status(), StatusCode::OK);
    assert!(body_string(root).await.contains("custom-root"));
    let response = get(&router, &builtin_url, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!body_string(response).await.contains("CUSTOM-SHADOW"));
}

#[tokio::test]
pub(super) async fn builtin_prefix_releases_ui_to_the_custom_owner() {
    let builtin_js = CONSOLE_JS;
    let custom_shadow = format!("_ui/{builtin_js}");
    let bundle = install_bundle(&[
        ("index.html", "<!doctype html><title>custom-root</title>"),
        ("ui/asset.js", "CUSTOM-UI-ASSET"),
        (&custom_shadow, "CUSTOM-BUILTIN-SHADOW"),
    ]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());

    let custom = get(&router, "/ui/asset.js", None).await;
    assert_eq!(custom.status(), StatusCode::OK);
    assert_eq!(body_string(custom).await, "CUSTOM-UI-ASSET");

    let custom_route = request(&router, "GET", "/ui", None, Some(BROWSER_ACCEPT)).await;
    assert_eq!(custom_route.status(), StatusCode::OK);
    assert!(body_string(custom_route).await.contains("custom-root"));

    let builtin = get(&router, &format!("/_ui/{builtin_js}"), None).await;
    assert_eq!(builtin.status(), StatusCode::OK);
    assert!(!body_string(builtin).await.contains("CUSTOM-BUILTIN-SHADOW"));
}

/// A product without a feature: that feature's routes are not served -- a
/// 404 from the API's own not-found handler, authenticated or not -- and
/// `meta` lists only what is served. The rest of the API is unchanged.
#[tokio::test]
pub(super) async fn a_feature_the_product_does_not_carry_is_not_served() {
    use micad_settings::{Feature, Features};
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    let router = app(AppState::new(fake, SIGNING_KEY)
        .with_features(Features::only(&[Feature::Mqtt, Feature::Containers])));
    let cookie = login(&router, "hunter2secret").await;

    for path in [
        "/api/v1/wifi/client",
        "/api/v1/wifi/ap",
        "/api/v1/wifi/client/networks",
        "/api/v1/bluetooth",
        "/api/v1/ssh/authorized-keys",
    ] {
        for credential in [None, Some(cookie.as_str())] {
            let response = get(&router, path, credential).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let body: serde_json::Value =
                serde_json::from_str(&body_string(response).await).expect("API error JSON");
            assert_eq!(body["error"]["code"], "not_found", "{path}");
        }
    }
    for path in ["/api/v1/mqtt", "/api/v1/containers", "/api/v1/network"] {
        let response = get(&router, path, Some(&cookie)).await;
        assert_ne!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    let meta: serde_json::Value =
        serde_json::from_str(&body_string(get(&router, "/api/v1/meta", Some(&cookie)).await).await)
            .expect("meta JSON");
    assert_eq!(meta["features"], json!(["containers", "mqtt"]));
}

/// With every feature, as on a development host, `meta` names all five.
#[tokio::test]
pub(super) async fn a_development_host_serves_every_feature() {
    let (router, _fake) = test_app(configured_tree("hunter2secret"));
    let cookie = login(&router, "hunter2secret").await;
    let meta: serde_json::Value =
        serde_json::from_str(&body_string(get(&router, "/api/v1/meta", Some(&cookie)).await).await)
            .expect("meta JSON");
    assert_eq!(
        meta["features"],
        json!(["wifi", "bluetooth", "ssh", "containers", "mqtt"])
    );
}

#[test]
pub(super) fn the_console_index_admits_the_tree_sorted_and_safe() {
    let builtin = crate::assets::builtin::Builtin::load(console())
        .unwrap()
        .expect("a console is installed");
    let paths = builtin.paths().collect::<Vec<_>>();
    let mut want = CONSOLE_FILES.map(|(path, _)| path).to_vec();
    want.sort_unstable();
    assert_eq!(paths, want);
}

/// A product that leaves out `mica-apid-ui` is an API-only device: `/_ui`
/// and `/` answer 404, and the API is exactly what it was.
#[tokio::test]
pub(super) async fn without_the_console_package_the_device_is_api_only() {
    let absent = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    let router = app(AppState::new(fake, SIGNING_KEY)
        .with_bundle_root(absent.path())
        .with_builtin_ui(&absent.path().join("no-console")));
    for path in [
        "/",
        "/_ui",
        "/_ui/",
        "/_ui/network",
        &format!("/_ui/{CONSOLE_JS}"),
    ] {
        let response = get(&router, path, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        get(&router, "/healthz", None).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&router, "/api/versions", None).await.status(),
        StatusCode::OK
    );
}

/// The console tree is indexed by rules, and a tree that breaks one is no
/// console rather than a partial one.
#[test]
pub(super) fn a_console_tree_that_breaks_a_rule_is_refused() {
    use crate::assets::builtin::Builtin;
    let absent = TempDir::new().unwrap();
    assert!(
        Builtin::load(&absent.path().join("missing"))
            .unwrap()
            .is_none()
    );

    let no_index = TempDir::new().unwrap();
    std::fs::write(no_index.path().join("app.js"), "x").unwrap();
    assert!(Builtin::load(no_index.path()).is_err());

    let linked = TempDir::new().unwrap();
    std::fs::write(linked.path().join("index.html"), "x").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", linked.path().join("passwd")).unwrap();
    assert!(Builtin::load(linked.path()).is_err());

    let unsafe_name = TempDir::new().unwrap();
    std::fs::write(unsafe_name.path().join("index.html"), "x").unwrap();
    std::fs::write(unsafe_name.path().join("back\\slash.js"), "x").unwrap();
    assert!(Builtin::load(unsafe_name.path()).is_err());
}

#[tokio::test]
pub(super) async fn built_in_vfs_serves_every_asset_with_owner_local_cache_rules() {
    let empty = TempDir::new().unwrap();
    let router = test_app_serving(configured_tree("hunter2secret"), empty.path());

    for (path, _) in CONSOLE_FILES {
        let response = get(&router, &format!("/_ui/{path}"), None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(response.headers().contains_key(CONTENT_TYPE), "{path}");
        assert_eq!(
            header_value(&response, CACHE_CONTROL),
            if path == "index.html" {
                "no-store"
            } else if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
            "{path}"
        );
        assert_eq!(
            header_value(&response, HeaderName::from_static("x-content-type-options")),
            "nosniff",
            "{path}"
        );
    }

    let fallback = get(&router, "/_ui/network", None).await;
    assert_eq!(fallback.status(), StatusCode::OK);
    assert_eq!(
        header_value(&fallback, CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    assert_eq!(header_value(&fallback, CACHE_CONTROL), "no-store");

    for path in ["/_ui/assets/missing.js", "/_ui/%252e%252e"] {
        let response = get(&router, path, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(header_value(&response, CACHE_CONTROL), "no-cache", "{path}");
        assert_eq!(body_string(response).await, "", "{path}");
    }
}
