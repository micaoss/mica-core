//! The fleet document and how it fails closed.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

#[tokio::test]
pub(super) async fn the_status_resolves_an_isolated_fleet_document_without_activity_state() {
    let (tree, token) = with_token(applied_tree());
    let (router, _meta) = provisioning_app_with_documents(
        tree,
        None,
        Some(
            r#"{
              "schema": "mica/fleet-config/v1",
              "enabled": true,
              "reporting": false,
              "url": "https://fleet.example.invalid"
            }"#,
        ),
    );

    let response = bearer(&router, "GET", STATUS_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let status = body_json(response).await;
    assert_eq!(
        status["operator"],
        json!({
            "fleet": {
                "enabled": true,
                "reporting": false,
                "url": "https://fleet.example.invalid",
            },
        })
    );
    assert_eq!(
        status["effective"]["fleet"],
        json!({
            "enabled": true,
            "reporting": false,
            "url": "https://fleet.example.invalid",
        })
    );
    for activity in ["registered", "connected", "lastReport", "credential"] {
        assert!(status["operator"]["fleet"].get(activity).is_none());
        assert!(status["effective"]["fleet"].get(activity).is_none());
    }
}

#[tokio::test]
pub(super) async fn fleet_url_absence_and_null_stay_distinct_while_both_use_baked() {
    let baked = BAKED_MANIFEST.replace(
        r#""fleet": { "enabled": false, "url": null }"#,
        r#""fleet": { "enabled": true, "url": "https://baked.example/fleet" }"#,
    );
    let cases = [
        (
            None,
            json!({}),
            json!({
                "enabled": true,
                "reporting": true,
                "url": "https://baked.example/fleet",
            }),
        ),
        (
            Some(r#"{ "schema": "mica/fleet-config/v1", "reporting": false }"#),
            json!({ "fleet": { "reporting": false } }),
            json!({
                "enabled": true,
                "reporting": false,
                "url": "https://baked.example/fleet",
            }),
        ),
        (
            Some(r#"{ "schema": "mica/fleet-config/v1", "reporting": false, "url": null }"#),
            json!({ "fleet": { "reporting": false, "url": null } }),
            json!({
                "enabled": true,
                "reporting": false,
                "url": "https://baked.example/fleet",
            }),
        ),
    ];

    for (document, expected_operator, expected_fleet) in cases {
        let (tree, token) = with_token(applied_tree());
        let (router, _meta) =
            provisioning_app_with_manifest_and_documents(tree, &baked, None, document);
        let status = body_json(bearer(&router, "GET", STATUS_PATH, &token).await).await;
        assert_eq!(status["operator"], expected_operator);
        assert_eq!(status["effective"]["fleet"], expected_fleet);
    }
}

#[tokio::test]
pub(super) async fn fleet_enabled_and_reporting_overrides_resolve_without_activity() {
    let cases = [
        (
            r#"{ "schema": "mica/fleet-config/v1", "enabled": true }"#,
            json!({ "fleet": { "enabled": true } }),
            json!({ "enabled": true, "reporting": true, "url": null }),
        ),
        (
            r#"{
              "schema": "mica/fleet-config/v1",
              "enabled": false,
              "reporting": true,
              "url": "https://fleet.example.invalid"
            }"#,
            json!({
                "fleet": {
                    "enabled": false,
                    "reporting": true,
                    "url": "https://fleet.example.invalid",
                },
            }),
            json!({
                "enabled": false,
                "reporting": false,
                "url": "https://fleet.example.invalid",
            }),
        ),
    ];

    for (document, expected_operator, expected_fleet) in cases {
        let (tree, token) = with_token(applied_tree());
        let (router, _meta) = provisioning_app_with_documents(tree, None, Some(document));
        let status = body_json(bearer(&router, "GET", STATUS_PATH, &token).await).await;
        assert_eq!(status["operator"], expected_operator);
        assert_eq!(status["effective"]["fleet"], expected_fleet);
        for activity in ["registered", "connected", "lastReport", "credential"] {
            assert!(status["operator"]["fleet"].get(activity).is_none());
            assert!(status["effective"]["fleet"].get(activity).is_none());
        }
    }
}

#[tokio::test]
pub(super) async fn valid_https_fleet_urls_preserve_the_complete_location() {
    for url in [
        "https://fleet.example.invalid/path?mode=test#fragment",
        "https://192.0.2.1:8443/report",
        "https://[2001:db8::1]/report?device=1",
    ] {
        let document = json!({ "schema": "mica/fleet-config/v1", "url": url }).to_string();
        let (tree, token) = with_token(applied_tree());
        let (router, _meta) = provisioning_app_with_documents(tree, None, Some(&document));
        let response = bearer(&router, "GET", STATUS_PATH, &token).await;
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        let status = body_json(response).await;
        assert_eq!(status["operator"], json!({ "fleet": { "url": url } }));
        assert_eq!(
            status["effective"]["fleet"],
            json!({ "enabled": false, "reporting": false, "url": url })
        );
    }
}

pub(super) async fn assert_fleet_document_rejected(document: &str, case: &str, hidden: &[&str]) {
    let (tree, token) = with_token(applied_tree());
    let (router, _meta) = provisioning_app_with_documents(tree, None, Some(document));
    let response = bearer(&router, "GET", STATUS_PATH, &token).await;
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "{case}"
    );
    let body = body_string(response).await;
    let error: serde_json::Value = serde_json::from_str(&body).expect("API error envelope");
    assert_eq!(
        error["error"]["code"], "configuration_unavailable",
        "{case}"
    );
    for rejected in hidden {
        assert!(
            !body.contains(rejected),
            "{case} disclosed rejected input: {body}"
        );
    }
}

#[tokio::test]
pub(super) async fn repair1_null_fleet_enabled_fails_closed_without_disclosure() {
    assert_fleet_document_rejected(
        r#"{ "schema": "mica/fleet-config/v1", "enabled": null }"#,
        "null enabled",
        &["enabled", "null"],
    )
    .await;
}

#[tokio::test]
pub(super) async fn repair1_null_fleet_reporting_fails_closed_without_disclosure() {
    assert_fleet_document_rejected(
        r#"{ "schema": "mica/fleet-config/v1", "reporting": null }"#,
        "null reporting",
        &["reporting", "null"],
    )
    .await;
}

#[tokio::test]
pub(super) async fn repair1_space_in_fleet_url_host_fails_closed_without_disclosure() {
    assert_fleet_document_rejected(
        r#"{ "schema": "mica/fleet-config/v1", "url": "https://bad host/REJECTED-FLEET-SENTINEL" }"#,
        "space in host",
        &["REJECTED-FLEET-SENTINEL"],
    )
    .await;
}

#[tokio::test]
pub(super) async fn repair1_invalid_bracketed_fleet_url_host_fails_closed_without_disclosure() {
    assert_fleet_document_rejected(
        r#"{ "schema": "mica/fleet-config/v1", "url": "https://[]/REJECTED-FLEET-SENTINEL" }"#,
        "invalid bracketed host",
        &["REJECTED-FLEET-SENTINEL"],
    )
    .await;
}

#[tokio::test]
pub(super) async fn repair1_fleet_url_userinfo_fails_closed_without_disclosure() {
    assert_fleet_document_rejected(
        r#"{ "schema": "mica/fleet-config/v1", "url": "https://REJECTED-FLEET-SENTINEL@fleet.example/path" }"#,
        "userinfo",
        &["REJECTED-FLEET-SENTINEL"],
    )
    .await;
}

#[tokio::test]
pub(super) async fn invalid_fleet_documents_fail_closed_without_disclosing_rejected_values() {
    const REJECTED: &str = "REJECTED-FLEET-SENTINEL";
    let documents = [
        (r#"[]"#, "non-object root"),
        (r#"{ "enabled": true }"#, "missing schema"),
        (
            r#"{ "schema": "REJECTED-FLEET-SENTINEL" }"#,
            "unsupported schema",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "enabled": "REJECTED-FLEET-SENTINEL" }"#,
            "incorrect enabled type",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "reporting": 7 }"#,
            "incorrect reporting type",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "url": true }"#,
            "incorrect URL type",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "url": "http://REJECTED-FLEET-SENTINEL" }"#,
            "non-HTTPS URL",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "unknown": "REJECTED-FLEET-SENTINEL" }"#,
            "unknown field",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "signingKeys": ["REJECTED-FLEET-SENTINEL"] }"#,
            "anchor field",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "enabled": false, "enabled": true }"#,
            "duplicate field",
        ),
        (
            r#"{ "schema": "mica/fleet-config/v1", "url": "REJECTED-FLEET-SENTINEL" "#,
            "malformed JSON",
        ),
    ];

    for (document, case) in documents {
        assert_fleet_document_rejected(document, case, &[REJECTED]).await;
    }
}

#[tokio::test]
pub(super) async fn unreadable_and_invalid_utf8_fleet_documents_fail_closed() {
    let (tree, token) = with_token(applied_tree());
    let (router, meta) = provisioning_app(tree);
    std::fs::create_dir_all(meta.path().join("config/fleet.json"))
        .expect("directory in place of fleet document");
    let response = bearer(&router, "GET", STATUS_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        envelope(response).await["code"],
        "configuration_unavailable"
    );

    let (tree, token) = with_token(applied_tree());
    let (router, meta) = provisioning_app(tree);
    std::fs::create_dir_all(meta.path().join("config")).expect("fleet config directory");
    std::fs::write(meta.path().join("config/fleet.json"), [0xff])
        .expect("invalid UTF-8 fleet document");
    let response = bearer(&router, "GET", STATUS_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        envelope(response).await["code"],
        "configuration_unavailable"
    );
}
