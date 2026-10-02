//! The web listener route.

use axum::http::StatusCode;
use serde_json::json;

use super::mqtt::{mqtt_session, mqtt_tree};
use super::*;
use crate::settings_api::SettingsApi;

const WEB_PATH: &str = "/api/v1/web";

/// A device tree carrying `access.web`, authenticated as the MQTT tree is.
fn web_tree(web: serde_json::Value) -> serde_json::Value {
    let mut tree = mqtt_tree(false);
    tree["access"]["web"] = web;
    tree
}

#[tokio::test]
pub(super) async fn the_web_read_answers_the_stored_listeners() {
    let web = json!({ "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 });
    let (router, _fake) = test_app(web_tree(web.clone()));
    let cookie = login(&router, "hunter2secret").await;

    let response = get(&router, WEB_PATH, Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, web);
}

/// The ports and the switch are one decision, written as one subtree.
#[tokio::test]
pub(super) async fn the_web_write_commits_the_whole_subtree_in_one_write() {
    let (router, fake) = test_app(web_tree(
        json!({ "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 }),
    ));
    let (cookie, csrf) = mqtt_session(&router).await;
    let moved = json!({ "httpPort": 8081, "httpsEnabled": true, "httpsPort": 9443 });

    let response = json_request(
        &router,
        "PUT",
        WEB_PATH,
        moved.clone(),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), vec!["access.web".to_string()]);
    assert_eq!(fake.get_settings("access.web").await.unwrap(), moved);
}

/// Port 0 and a partial body are refused before micad is asked.
#[tokio::test]
pub(super) async fn the_web_write_refuses_port_zero_and_a_partial_body() {
    let (router, fake) = test_app(web_tree(
        json!({ "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 }),
    ));
    let (cookie, csrf) = mqtt_session(&router).await;
    for body in [
        json!({ "httpPort": 0, "httpsEnabled": false, "httpsPort": 8443 }),
        json!({ "httpPort": 8080 }),
    ] {
        let response =
            json_request(&router, "PUT", WEB_PATH, body, Some(&cookie), Some(&csrf)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert!(fake.set_paths().is_empty());
}

/// Over plain HTTP a `Secure` cookie would be dropped by the browser, so the
/// session cookie carries it exactly when the console is served over HTTPS.
#[test]
pub(super) fn the_session_cookie_is_secure_only_over_https() {
    assert!(crate::session::session_cookie("v", true).contains("; Secure;"));
    assert!(!crate::session::session_cookie("v", false).contains("Secure"));
    assert!(!crate::session::clear_cookie(false).contains("Secure"));
}

/// An app whose TLS identity lives in `dir`.
fn identity_app(dir: &std::path::Path) -> axum::Router {
    let fake = std::sync::Arc::new(crate::settings_api::FakeSettings::new(mqtt_tree(false)));
    let state = crate::routes::AppState::new(fake, SIGNING_KEY)
        .with_builtin_ui(console())
        .with_persistence(dir);
    crate::routes::app(state)
}

/// The identity PEM of `request`, split into the chain and the key.
fn generated_pair(request: &crate::tls::CertificateRequest) -> (String, String) {
    let identity = crate::tls::generate_identity(request).unwrap();
    let at = identity.find("-----BEGIN PRIVATE KEY-----").unwrap();
    (identity[..at].to_string(), identity[at..].to_string())
}

#[tokio::test]
pub(super) async fn a_device_that_never_served_https_has_no_certificate_yet() {
    let dir = tempfile::tempdir().unwrap();
    let router = identity_app(dir.path());
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, CERTIFICATE_PATH, Some(&cookie)).await).await;
    assert_eq!(body, json!({ "serving": false, "certificate": null }));
}

/// A generated certificate carries the names asked for and replaces the
/// stored identity; the key never comes back.
#[tokio::test]
pub(super) async fn a_generated_certificate_carries_the_names_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let router = identity_app(dir.path());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "POST",
        GENERATE_PATH,
        json!({
            "commonName": "edge-1",
            "dnsNames": ["edge-1.local", "Edge-1"],
            "ipAddresses": ["192.168.1.20"],
            "validityDays": 365,
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["subject"], json!("CN=edge-1"));
    assert_eq!(body["selfSigned"], json!(true));
    assert_eq!(body["dnsNames"], json!(["edge-1.local", "edge-1"]));
    assert_eq!(body["ipAddresses"], json!(["192.168.1.20"]));
    assert!(body.get("privateKey").is_none());

    let stored = std::fs::read_to_string(dir.path().join("identity.pem")).unwrap();
    assert_eq!(
        crate::tls::describe_identity(&stored).unwrap().sha256,
        body["sha256"].as_str().unwrap()
    );
    let read = body_json(get(&router, CERTIFICATE_PATH, Some(&cookie)).await).await;
    assert_eq!(read["certificate"], body);

    for refused in [
        json!({ "commonName": "", "dnsNames": ["a"], "validityDays": 1 }),
        json!({ "commonName": "a", "dnsNames": ["bad name"], "validityDays": 1 }),
        json!({ "commonName": "a", "ipAddresses": ["300.1.1.1"], "validityDays": 1 }),
        json!({ "commonName": "a", "validityDays": 1 }),
        json!({ "commonName": "a", "dnsNames": ["a"], "validityDays": 0 }),
    ] {
        let response = json_request(
            &router,
            "POST",
            GENERATE_PATH,
            refused.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{refused}"
        );
    }
}

/// An uploaded pair is stored when the key is the certificate's, and refused
/// when it is not, leaving the stored identity alone.
#[tokio::test]
pub(super) async fn an_uploaded_certificate_must_come_with_its_own_key() {
    let dir = tempfile::tempdir().unwrap();
    let router = identity_app(dir.path());
    let (cookie, csrf) = mqtt_session(&router).await;
    let request = crate::tls::CertificateRequest {
        common_name: "uploaded".to_string(),
        ..crate::tls::CertificateRequest::default()
    };
    let (chain, key) = generated_pair(&request);
    let (_, other_key) = generated_pair(&request);

    let upload = |certificate: &str, private_key: &str| json!({ "certificate": certificate, "privateKey": private_key });
    let response = json_request(
        &router,
        "PUT",
        CERTIFICATE_PATH,
        upload(&chain, &other_key),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!dir.path().join("identity.pem").exists());

    let response = json_request(
        &router,
        "PUT",
        CERTIFICATE_PATH,
        upload(&chain, &key),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["subject"], json!("CN=uploaded"));
    let stored = std::fs::read_to_string(dir.path().join("identity.pem")).unwrap();
    assert!(stored.contains("BEGIN CERTIFICATE") && stored.contains("BEGIN PRIVATE KEY"));

    let response = json_request(
        &router,
        "PUT",
        CERTIFICATE_PATH,
        upload("not pem", &key),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

const CERTIFICATE_PATH: &str = "/api/v1/web/certificate";
const GENERATE_PATH: &str = "/api/v1/web/certificate/generate";
