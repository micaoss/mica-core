//! The container routes.

use crate::auth;
use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

pub(super) const CONTAINERS_API: &str = "/api/v1/containers";

// A device with containers switched on and one declared.
pub(super) fn container_tree() -> serde_json::Value {
    let hash = auth::hash_password("hunter2secret").unwrap();
    json!({
        "hostname": "mica",
        "network": {},
        "access": { "webAdmin": { "password_hash": hash } },
        "container": {
            "enabled": true,
            "units": {
                "node-red": {
                    "image": "docker.io/nodered/node-red:4.0.9",
                    "autoStart": true,
                },
            },
        },
    })
}

// A declaration replaces the entry whole and answers the apply task.
#[tokio::test]
pub(super) async fn declaring_a_container_writes_the_map_and_answers_a_task() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        &format!("{CONTAINERS_API}/metrics"),
        json!({
            "image": "docker.io/library/busybox:1",
            "publish": [{ "host": 9100, "container": 9100 }],
            "volumes": [{ "host": "/mica/apps/metrics", "container": "/data" }],
            "restart": "always",
            "autoStart": true,
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), vec!["container.units".to_string()]);
    let stored = fake.get_settings("container.units").await.unwrap();
    // The entry that was already there is untouched: this route replaces one
    // container, not the map.
    assert!(stored["node-red"].is_object(), "{stored}");
    assert_eq!(stored["metrics"]["publish"][0]["host"], json!(9100));
}

// The device's own rules, answered here rather than as a bus error.
#[tokio::test]
pub(super) async fn a_container_the_device_would_refuse_is_refused_here() {
    for (name, body) in [
        // A volume outside the operator's half of the device.
        (
            "app",
            json!({ "image": "alpine:3", "volumes": [{ "host": "/etc", "container": "/host" }] }),
        ),
        // A host port a declared container already publishes.
        (
            "app",
            json!({ "image": "alpine:3", "publish": [{ "host": 1880, "container": 1880 }] }),
        ),
        // A name no unit could carry, percent-encoded as a client would
        // have to send it.
        ("node%20red", json!({ "image": "alpine:3" })),
        // No image.
        ("app", json!({ "image": "" })),
    ] {
        let mut tree = container_tree();
        tree["container"]["units"]["node-red"]["publish"] =
            json!([{ "host": 1880, "container": 1880 }]);
        let (router, fake) = test_app(tree);
        let (cookie, csrf) = mqtt_session(&router).await;

        let response = json_request(
            &router,
            "PUT",
            &format!("{CONTAINERS_API}/{name}"),
            body.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name}: {body}"
        );
        assert!(
            fake.set_paths().is_empty(),
            "a refused container was written"
        );
    }
}

// Removing one re-validates the rest without it, and a name nobody declared is
// a 404 rather than a silent success.
#[tokio::test]
pub(super) async fn removing_a_container_takes_only_that_entry() {
    let (router, fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let absent = json_request(
        &router,
        "DELETE",
        &format!("{CONTAINERS_API}/nothing"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    assert!(fake.set_paths().is_empty());

    let response = json_request(
        &router,
        "DELETE",
        &format!("{CONTAINERS_API}/node-red"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = fake.get_settings("container.units").await.unwrap();
    assert_eq!(stored, json!({}));
}

// The action path takes three verbs and nothing else.
#[tokio::test]
pub(super) async fn a_container_action_path_serves_three_verbs_and_no_others() {
    let (router, _fake) = test_app(container_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "POST",
        &format!("{CONTAINERS_API}/node-red/destroy"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // The settings collection's own not-found code: the path names a place in
    // the tree, and `destroy` is not one.
    assert_eq!(envelope(response).await["code"], "settings_not_found");
}

// --- Routes and a DHCP server on a declared interface ------------------------
