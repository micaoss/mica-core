//! Custom UI bundles: upload, selection and the data reserve.

use crate::bundle::Store;
use crate::routes::{AppState, app};
use crate::settings_api::FakeSettings;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

use super::*;

#[tokio::test]
pub(super) async fn ui_selection_is_managed_only_through_the_csrf_protected_api() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom-root</title>")]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
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
    let body: serde_json::Value =
        serde_json::from_str(&body_string(login).await).expect("session JSON");
    let csrf = body["csrfToken"].as_str().unwrap();

    let status = get(&router, "/api/v1/ui", Some(&cookie)).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status: serde_json::Value =
        serde_json::from_str(&body_string(status).await).expect("UI status JSON");
    assert_eq!(status["mode"], "custom");
    assert_eq!(status["custom"]["generation"], 1);
    assert_eq!(status["availableCustom"]["generation"], 1);
    assert_eq!(status["availableCustom"]["usable"], true);

    let refused = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/active",
        serde_json::Value::Null,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let accepted = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/active",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted: serde_json::Value =
        serde_json::from_str(&body_string(accepted).await).expect("UI status JSON");
    assert_eq!(accepted["mode"], "builtIn");
    assert_eq!(accepted["availableCustom"]["generation"], 1);
    assert_eq!(accepted["availableCustom"]["usable"], true);
    assert_eq!(
        get(&router, "/", None).await.status(),
        StatusCode::SEE_OTHER
    );

    let refused = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let accepted = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted: serde_json::Value =
        serde_json::from_str(&body_string(accepted).await).expect("UI status JSON");
    assert_eq!(accepted["mode"], "custom");
    assert_eq!(accepted["custom"]["generation"], 1);
    let root = get(&router, "/", None).await;
    assert_eq!(root.status(), StatusCode::OK);
    assert!(body_string(root).await.contains("custom-root"));
}

pub(super) fn ui_package_bytes(api_version: &str) -> Vec<u8> {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join("assets")).unwrap();
    std::fs::write(
        source.join("index.html"),
        "<!doctype html><title>uploaded</title>",
    )
    .unwrap();
    std::fs::write(source.join("assets/app.js"), "console.log('uploaded')").unwrap();
    std::fs::write(
        source.join("mica-ui.json"),
        serde_json::to_vec(&json!({
            "schemaVersion": 1,
            "name": "uploaded",
            "version": "1.0.0",
            "immutableDir": "assets",
            "apiVersions": [api_version],
        }))
        .unwrap(),
    )
    .unwrap();
    let package = temp.path().join("uploaded.mica-ui.zip");
    mica_ui_bundle::pack(&source, &package).unwrap();
    std::fs::read(package).unwrap()
}

/// The DATA reserve is the signed boot policy's `board.reserves.data`; a
/// policy that declares none, or one that cannot be read as a policy, keeps
/// the default the deployment client keeps.
#[test]
pub(super) fn the_data_reserve_is_the_boot_policy_s() {
    const MIB: u64 = 1024 * 1024;
    let declared = json!({"board": {"reserves": {"data": 8 * MIB}}}).to_string();
    assert_eq!(crate::routes::data_reserve(Some(&declared)), 8 * MIB);
    for undeclared in [
        json!({"board": {"boot": "uefi"}}).to_string(),
        json!({"board": {"reserves": {"system": 4 * MIB}}}).to_string(),
        json!({"board": {"reserves": {"data": MIB - 1}}}).to_string(),
        json!({"board": {"reserves": {"data": "8M"}}}).to_string(),
        "not json".to_string(),
    ] {
        assert_eq!(
            crate::routes::data_reserve(Some(&undeclared)),
            128 * MIB,
            "{undeclared}"
        );
    }
    assert_eq!(crate::routes::data_reserve(None), 128 * MIB);
}

#[tokio::test]
pub(super) async fn a_ui_package_is_refused_when_extraction_would_reach_the_data_reserve() {
    let bundle_root = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    // More than any test filesystem has free: the reserve, not the disk, refuses.
    let reserve = 1 << 60;
    let router = app(AppState::new(fake, SIGNING_KEY)
        .with_bundle_root(bundle_root.path())
        .with_data_reserve(reserve));
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
    let body: serde_json::Value = serde_json::from_str(&body_string(login).await).unwrap();
    let csrf = body["csrfToken"].as_str().unwrap();
    let refused = zip_request(
        &router,
        "/api/v1/ui/bundles",
        ui_package_bytes("v1"),
        &cookie,
        Some(csrf),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::INSUFFICIENT_STORAGE);
    let refused: serde_json::Value = serde_json::from_str(&body_string(refused).await).unwrap();
    assert_eq!(refused["error"]["code"], "ui_storage_headroom");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&format!("{} MiB", reserve / (1024 * 1024))),
        "{refused}"
    );
}

#[tokio::test]
pub(super) async fn ui_package_upload_installs_without_activation_and_supports_explicit_lifecycle()
{
    let bundle_root = TempDir::new().unwrap();
    let router = test_app_serving(configured_tree("hunter2secret"), bundle_root.path());
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
    let body: serde_json::Value = serde_json::from_str(&body_string(login).await).unwrap();
    let csrf = body["csrfToken"].as_str().unwrap();
    let package = ui_package_bytes("v1");

    let missing_csrf = zip_request(
        &router,
        "/api/v1/ui/bundles",
        package.clone(),
        &cookie,
        None,
    )
    .await;
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);

    let incompatible = zip_request(
        &router,
        "/api/v1/ui/bundles",
        ui_package_bytes("v999"),
        &cookie,
        Some(csrf),
    )
    .await;
    assert_eq!(incompatible.status(), StatusCode::CONFLICT);
    let incompatible: serde_json::Value =
        serde_json::from_str(&body_string(incompatible).await).unwrap();
    assert_eq!(incompatible["error"]["code"], "ui_package_conflict");

    let uploaded = zip_request(
        &router,
        "/api/v1/ui/bundles",
        package.clone(),
        &cookie,
        Some(csrf),
    )
    .await;
    assert_eq!(uploaded.status(), StatusCode::CREATED);
    let uploaded: serde_json::Value = serde_json::from_str(&body_string(uploaded).await).unwrap();
    assert!(uploaded["activeGeneration"].is_null());
    assert_eq!(uploaded["bundles"][0]["generation"], 1);
    assert_eq!(uploaded["bundles"][0]["name"], "uploaded");
    assert_eq!(uploaded["bundles"][0]["digest"].as_str().unwrap().len(), 64);
    assert!(uploaded["bundles"][0]["compressedBytes"].as_u64().unwrap() > 0);
    assert!(uploaded["bundles"][0]["expandedBytes"].as_u64().unwrap() > 0);
    assert_eq!(
        get(&router, "/", None).await.status(),
        StatusCode::SEE_OTHER
    );

    let duplicate = zip_request(&router, "/api/v1/ui/bundles", package, &cookie, Some(csrf)).await;
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let activated = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(activated.status(), StatusCode::OK);
    assert!(
        body_string(get(&router, "/", None).await)
            .await
            .contains("uploaded")
    );

    let active_delete = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/bundles/1",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(active_delete.status(), StatusCode::CONFLICT);

    let deactivated = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/active",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(deactivated.status(), StatusCode::OK);
    let deleted = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/bundles/1",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
pub(super) async fn the_ui_api_refuses_a_corrupt_retained_bundle_without_changing_root() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom</title>")]);
    let router = test_app_serving(configured_tree("hunter2secret"), bundle.path());
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
    let body: serde_json::Value =
        serde_json::from_str(&body_string(login).await).expect("session JSON");
    let csrf = body["csrfToken"].as_str().unwrap();

    let deactivated = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/active",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(deactivated.status(), StatusCode::OK);
    std::fs::write(
        Store::new(bundle.path()).bundle_dir(1).join("index.html"),
        "changed",
    )
    .expect("corrupt retained bundle");

    let status = get(&router, "/api/v1/ui", Some(&cookie)).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status: serde_json::Value =
        serde_json::from_str(&body_string(status).await).expect("UI status JSON");
    assert_eq!(status["availableCustom"]["usable"], false);
    assert_eq!(
        status["availableCustom"]["unavailableReason"],
        "digestMismatch"
    );

    let refused = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let refused: serde_json::Value =
        serde_json::from_str(&body_string(refused).await).expect("API error JSON");
    assert_eq!(refused["error"]["code"], "custom_ui_unavailable");
    assert_eq!(
        get(&router, "/", None).await.status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
pub(super) async fn ui_selection_records_deactivation_activation_and_no_op() {
    let bundle = install_bundle(&[("index.html", "<!doctype html><title>custom</title>")]);
    let audit_dir = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(configured_tree("hunter2secret")));
    let router = app(AppState::new(fake, SIGNING_KEY)
        .with_persistence(audit_dir.path())
        .with_bundle_root(bundle.path()));
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
    let body: serde_json::Value =
        serde_json::from_str(&body_string(login).await).expect("session JSON");
    let csrf = body["csrfToken"].as_str().unwrap();

    let response = json_request(
        &router,
        "DELETE",
        "/api/v1/ui/active",
        serde_json::Value::Null,
        Some(&cookie),
        Some(csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "DELETE");

    let first = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        Some(csrf),
    );
    let second = json_request(
        &router,
        "PUT",
        "/api/v1/ui/active",
        json!({ "generation": 1 }),
        Some(&cookie),
        Some(csrf),
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.status(), StatusCode::OK, "first parallel PUT");
    assert_eq!(second.status(), StatusCode::OK, "second parallel PUT");

    let outcomes: Vec<String> = audit_lines(audit_dir.path())
        .into_iter()
        .filter(|line| line["event"] == "custom-ui")
        .map(|line| line["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes, ["deactivated", "activated", "no-op"]);
}
