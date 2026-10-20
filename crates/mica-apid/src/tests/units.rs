//! The user's services' routes.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::containers::container_tree;
use super::*;

const UNITS_API: &str = "/api/v1/units";
const SERVICE: &str = "[Service]\nExecStart=/srv/x\n[Install]\nWantedBy=multi-user.target\n";

// A service is stored under its quoted name, as the body gives it; a body of
// another shape is a 422 and writes nothing.
#[tokio::test]
pub(super) async fn declaring_a_service_stores_it_under_its_name() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let refused = json_request(
        &router,
        "PUT",
        &format!("{UNITS_API}/x.service"),
        json!({ "text": SERVICE, "enabled": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(fake.set_paths().is_empty());

    let response = json_request(
        &router,
        "PUT",
        &format!("{UNITS_API}/x.service"),
        json!({ "kind": "systemd", "text": SERVICE, "enabled": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(
        fake.set_paths(),
        vec!["services.units.\"x.service\"".to_string()]
    );
}

// Removing a service takes only it; one nobody declared is a 404.
#[tokio::test]
pub(super) async fn removing_a_service_takes_only_it() {
    let mut tree = container_tree();
    let unit = json!({ "kind": "openrc", "text": "#!/sbin/openrc-run\n", "enabled": false });
    tree["services"] = json!({ "units": { "a": unit, "b": unit } });
    let (router, fake) = test_app(tree);
    let (cookie, csrf) = mqtt_session(&router).await;

    for (name, status) in [
        ("nothing", StatusCode::NOT_FOUND),
        ("a", StatusCode::ACCEPTED),
    ] {
        let response = json_request(
            &router,
            "DELETE",
            &format!("{UNITS_API}/{name}"),
            json!({}),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), status, "{name}");
    }
    assert_eq!(
        fake.get_settings("services.units").await.unwrap(),
        json!({ "b": unit })
    );
}

// The read is micad's; a verb reaches micad, and one that is not one of the
// three is a 404.
#[tokio::test]
pub(super) async fn the_services_are_read_and_driven_through_micad() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let read = get(&router, UNITS_API, Some(&cookie)).await;
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(body_json(read).await["units"][0]["name"], "x.service");

    for (path, status) in [
        ("x.service/restart", StatusCode::NO_CONTENT),
        ("x.service/destroy", StatusCode::NOT_FOUND),
    ] {
        let response = json_request(
            &router,
            "POST",
            &format!("{UNITS_API}/{path}"),
            json!({}),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), status, "{path}");
    }
    assert_eq!(
        fake.container_calls(),
        vec!["unit restart x.service".to_string()]
    );
}

// A service's log is scrubbed before it leaves.
#[tokio::test]
pub(super) async fn a_service_log_is_scrubbed() {
    let (router, _fake) = test_app(container_tree());
    let (cookie, _csrf) = mqtt_session(&router).await;

    let response = get(
        &router,
        &format!("{UNITS_API}/x.service/logs"),
        Some(&cookie),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let log = body_json(response).await;
    assert_eq!(log["lines"][0], "started");
    assert!(!log.to_string().contains("hunter2"), "{log}");
}
