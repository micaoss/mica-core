//! The container routes.

use crate::auth;
use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

// A device with containers switched on and one project declared.
pub(super) fn container_tree() -> serde_json::Value {
    let hash = auth::hash_password("hunter2secret").unwrap();
    json!({
        "hostname": "mica",
        "network": {},
        "access": { "webAdmin": { "password_hash": hash } },
        "container": {
            "enabled": true,
            "projects": { "app": "services:\n  web:\n    image: nginx:1\n" },
        },
    })
}

pub(super) const PROJECTS_API: &str = "/api/v1/container-projects";

// A project is checked, then stored under its name, and answers the task.
#[tokio::test]
pub(super) async fn declaring_a_project_checks_and_stores_it() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let compose = "services:\n  web:\n    image: nginx:1\n";

    let response = json_request(
        &router,
        "PUT",
        &format!("{PROJECTS_API}/app"),
        json!({ "compose": compose }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), vec!["container.projects.app".to_string()]);
    assert_eq!(
        fake.get_settings("container.projects.app").await.unwrap(),
        json!(compose)
    );
}

// A file mica-containerd refuses is a 422 carrying every problem, and nothing
// is written; so is a name no project can have.
#[tokio::test]
pub(super) async fn a_refused_project_names_its_problems_and_writes_nothing() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        &format!("{PROJECTS_API}/app"),
        json!({ "compose": "services:\n  web:\n    privileged: true\n" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "compose_invalid");
    assert_eq!(error["problems"][0]["service"], "web");
    assert_eq!(error["problems"][0]["field"], "privileged");

    for (name, body) in [
        ("App", json!({ "compose": "services: {}" })),
        ("app", json!({ "file": "services: {}" })),
    ] {
        let response = json_request(
            &router,
            "PUT",
            &format!("{PROJECTS_API}/{name}"),
            body,
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name}"
        );
        assert_eq!(envelope(response).await["code"], "validation_failed");
    }
    assert!(fake.set_paths().is_empty());
}

// The check stores nothing and answers what it found.
#[tokio::test]
pub(super) async fn a_check_stores_nothing() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    for (compose, ok) in [
        ("services:\n  web:\n    image: nginx:1\n", true),
        ("services:\n  web:\n    privileged: true\n", false),
    ] {
        let response = json_request(
            &router,
            "POST",
            &format!("{PROJECTS_API}/app/check"),
            json!({ "compose": compose }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], json!(ok), "{body}");
        assert_eq!(body["problems"].is_array(), !ok, "{body}");
    }
    assert!(fake.set_paths().is_empty());
}

// Removing a project takes only it; one nobody declared is a 404.
#[tokio::test]
pub(super) async fn removing_a_project_takes_only_it() {
    let mut tree = container_tree();
    tree["container"]["projects"] = json!({ "app": "a", "db": "b" });
    let (router, fake) = test_app(tree);
    let (cookie, csrf) = mqtt_session(&router).await;

    let absent = json_request(
        &router,
        "DELETE",
        &format!("{PROJECTS_API}/nothing"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);

    let response = json_request(
        &router,
        "DELETE",
        &format!("{PROJECTS_API}/app"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.get_settings("container.projects").await.unwrap(),
        json!({ "db": "b" })
    );
}

// A project's verbs and a service's reach micad as the names it resolves; a
// verb that is not one of the three is a 404.
#[tokio::test]
pub(super) async fn project_and_service_verbs_reach_micad() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    for (path, status) in [
        ("app/stop", StatusCode::NO_CONTENT),
        ("app/services/web/restart", StatusCode::NO_CONTENT),
        ("app/destroy", StatusCode::NOT_FOUND),
        ("app/services/web/destroy", StatusCode::NOT_FOUND),
    ] {
        let response = json_request(
            &router,
            "POST",
            &format!("{PROJECTS_API}/{path}"),
            json!({}),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), status, "{path}");
    }
    assert_eq!(
        fake.container_calls(),
        vec!["stop app".to_string(), "restart app/web".to_string()]
    );
}

// A service's log is read through micad and scrubbed before it leaves.
#[tokio::test]
pub(super) async fn a_service_log_is_scrubbed() {
    let (router, fake) = test_app(container_tree());
    let (cookie, _csrf) = mqtt_session(&router).await;
    let response = get(
        &router,
        &format!("{PROJECTS_API}/app/services/web/logs?tail=20"),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let log = body_json(response).await["log"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(log.starts_with("started\n"), "{log}");
    assert!(!log.contains("hunter2"), "{log}");
    assert_eq!(fake.container_calls(), vec!["log app/web 20".to_string()]);
}

// A Docker name (capitals, dots, more than 40 characters) reaches micad as
// it is, for micad to resolve against dockerd's containers.
#[tokio::test]
pub(super) async fn docker_names_reach_micad() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let long = format!("Portainer.CE_{}", "x".repeat(40));
    for path in [
        format!("{long}/restart"),
        "My.App/services/Web.Front_1/stop".to_string(),
    ] {
        let response = json_request(
            &router,
            "POST",
            &format!("{PROJECTS_API}/{path}"),
            json!({}),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
    }
    let response = get(
        &router,
        &format!("{PROJECTS_API}/My.App/services/Web.Front_1/logs?tail=5"),
        Some(&cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fake.container_calls(),
        vec![
            format!("restart {long}"),
            "stop My.App/Web.Front_1".to_string(),
            "log My.App/Web.Front_1 5".to_string(),
        ]
    );
}

// --- Routes and a DHCP server on a declared interface ------------------------
