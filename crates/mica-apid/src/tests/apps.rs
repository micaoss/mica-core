//! The apps' routes: the upload, the actions and their error names. What a
//! bundle may hold and what each flow does is micad's, tested in micad.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

const APPS: &str = "/api/v1/apps";

fn apps_app(uploads: &TempDir) -> (Router, Arc<FakeSettings>, String) {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_app_uploads(uploads.path()));
    (router, fake, token)
}

fn left(uploads: &TempDir) -> usize {
    std::fs::read_dir(uploads.path()).unwrap().count()
}

/// An upload is written, named to micad and answered with its task.
#[tokio::test]
async fn an_uploaded_bundle_is_handed_to_micad() {
    let uploads = TempDir::new().unwrap();
    let (router, fake, token) = apps_app(&uploads);
    let response = bearer_bytes(
        &router,
        &format!("{APPS}?reinstall=true"),
        b"\x1f\x8b a bundle".to_vec(),
        "application/octet-stream",
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.container_calls(), ["app install 11 reinstall=true"]);
    assert_eq!(left(&uploads), 0, "micad took the file");
}

/// micad's refusals keep their names: the rules a bundle breaks, a busy
/// device, and a body that is not a bundle upload at all. Nothing is left.
#[tokio::test]
async fn a_refused_bundle_names_why_and_leaves_nothing() {
    let uploads = TempDir::new().unwrap();
    let (router, _fake, token) = apps_app(&uploads);
    let invalid = bearer_bytes(
        &router,
        APPS,
        b"bad".to_vec(),
        "application/octet-stream",
        &token,
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(invalid).await;
    assert_eq!(body["error"]["code"], json!("app_invalid"));
    assert_eq!(
        body["error"]["rules"],
        json!([{ "rule": "setuid", "detail": "files/su is setuid or setgid" }])
    );
    let busy = bearer_bytes(
        &router,
        APPS,
        b"busy".to_vec(),
        "application/octet-stream",
        &token,
    )
    .await;
    assert_eq!(busy.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(busy).await["error"]["code"], json!("app_busy"));
    let typed = bearer_bytes(&router, APPS, b"x".to_vec(), "application/json", &token).await;
    assert_eq!(typed.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(left(&uploads), 0);
}

/// A bundle that asks for privileges beyond the installed version's is a 409
/// naming them, and nothing is left; with `acceptPrivileges` it is taken.
#[tokio::test]
async fn a_bundle_asking_for_privileges_is_asked_about_first() {
    let uploads = TempDir::new().unwrap();
    let (router, fake, token) = apps_app(&uploads);
    let asked = bearer_bytes(
        &router,
        APPS,
        b"root".to_vec(),
        "application/octet-stream",
        &token,
    )
    .await;
    assert_eq!(asked.status(), StatusCode::CONFLICT);
    let body = body_json(asked).await;
    assert_eq!(body["error"]["code"], json!("app_privileges"));
    assert_eq!(
        body["error"]["params"],
        json!({ "root": true, "capabilities": [], "devices": [] })
    );
    assert_eq!(left(&uploads), 0);
    let accepted = bearer_bytes(
        &router,
        &format!("{APPS}?acceptPrivileges=true"),
        b"root".to_vec(),
        "application/octet-stream",
        &token,
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.container_calls(),
        [
            "app install 4 reinstall=false",
            "app install 4 reinstall=false"
        ]
    );
}

/// The reads, the task actions, the verbs and the removal reach micad; an
/// app that is not installed is a 404 and an unknown action is too.
#[tokio::test]
async fn the_actions_reach_micad() {
    let uploads = TempDir::new().unwrap();
    let (router, fake, token) = apps_app(&uploads);
    let list = bearer(&router, "GET", APPS, &token).await;
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(body_json(list).await["apps"][0]["name"], json!("demo"));
    for (method, path, status) in [
        ("GET", "/demo", StatusCode::OK),
        ("POST", "/demo/rollback", StatusCode::ACCEPTED),
        ("POST", "/demo/check", StatusCode::ACCEPTED),
        ("POST", "/demo/update", StatusCode::ACCEPTED),
        (
            "POST",
            "/demo/update?acceptPrivileges=true",
            StatusCode::ACCEPTED,
        ),
        ("POST", "/demo/restart", StatusCode::NO_CONTENT),
        ("DELETE", "/demo?purge=true", StatusCode::ACCEPTED),
        ("POST", "/demo/explode", StatusCode::NOT_FOUND),
        ("GET", "/other", StatusCode::NOT_FOUND),
        ("POST", "/other/stop", StatusCode::NOT_FOUND),
    ] {
        let response = bearer(&router, method, &format!("{APPS}{path}"), &token).await;
        assert_eq!(response.status(), status, "{method} {path}");
    }
    assert_eq!(
        fake.container_calls(),
        [
            "app rollback demo",
            "app check demo",
            "app update demo",
            "app update demo accepted",
            "app restart demo",
            "app remove demo purge=true",
        ]
    );
    let missing = bearer(&router, "GET", &format!("{APPS}/other"), &token).await;
    assert_eq!(
        body_json(missing).await["error"]["code"],
        json!("app_not_found")
    );
}

/// Every log line leaves the device scrubbed, the first-run output too.
#[tokio::test]
async fn an_apps_log_is_scrubbed() {
    let uploads = TempDir::new().unwrap();
    let (router, _fake, token) = apps_app(&uploads);
    let response = bearer(&router, "GET", &format!("{APPS}/demo/logs"), &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let text = body.to_string();
    assert!(!text.contains("hunter2"), "{text}");
    assert_eq!(body["units"][0]["lines"][0], json!("started"));
    assert_eq!(body["firstRun"][0], json!("joined"));
}
