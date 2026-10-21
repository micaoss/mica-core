//! The Bluetooth routes.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

pub(super) const BLUETOOTH_API: &str = "/api/v1/bluetooth";

// A device with a Bluetooth subtree to write over.
pub(super) fn bluetooth_tree() -> serde_json::Value {
    let mut tree = configured_tree("hunter2secret");
    tree["bluetooth"] = json!({
        "enabled": true,
        "pin": "421137",
        "devices": { "AA:BB:CC:DD:EE:01": { "name": "phone", "trusted": true, "blocked": false } },
    });
    tree
}

// The read is micad's document: the two sides named apart, the pairing window
// and what waits in it.
#[tokio::test]
pub(super) async fn the_bluetooth_read_answers_micads_document() {
    let (router, _fake) = test_app(bluetooth_tree());
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, BLUETOOTH_API, Some(&cookie)).await).await;

    assert_eq!(body["adapter"]["available"], json!(true));
    assert_eq!(body["adapter"]["name"], json!("edge-42"));
    assert_eq!(
        body["declared"]["AA:BB:CC:DD:EE:01"]["trusted"],
        json!(true)
    );
    assert_eq!(body["pending"], json!([]));
    assert!(body["pairWindow"].is_null());
}

// The adapter write is the switch, and it keeps the trust list and the app
// code: devices arrive by pairing, and the code is mica-bled's.
#[tokio::test]
pub(super) async fn the_adapter_write_is_the_switch_alone() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        BLUETOOTH_API,
        json!({ "powered": false }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec!["bluetooth.enabled".to_string()]);
    let stored = fake.get_settings("bluetooth").await.unwrap();
    assert_eq!(stored["enabled"], json!(false));
    assert_eq!(stored["pin"], json!("421137"));
    assert_eq!(
        stored["devices"]["AA:BB:CC:DD:EE:01"]["name"],
        json!("phone")
    );
}

// What the adapter no longer takes is named, not silently dropped.
#[tokio::test]
pub(super) async fn a_retired_adapter_field_is_not_supported() {
    for field in ["discoverable", "alias", "pairingCode", "pin"] {
        let (router, fake) = test_app(bluetooth_tree());
        let (cookie, csrf) = mqtt_session(&router).await;
        let mut body = json!({ "powered": true });
        body[field] = json!(true);

        let response = json_request(
            &router,
            "PUT",
            BLUETOOTH_API,
            body,
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], json!("not_supported"), "{field}");
        assert_eq!(error["params"]["field"], json!(field));
        assert!(fake.set_paths().is_empty());
    }
}

// The window opens, shows who asks, takes an answer, and closes.
#[tokio::test]
pub(super) async fn the_pairing_window_and_its_requests() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let window = format!("{BLUETOOTH_API}/pairing-window");

    let opened = json_request(
        &router,
        "POST",
        &window,
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(opened.status(), StatusCode::OK);
    let expires_at = body_json(opened).await["expiresAt"]
        .as_u64()
        .expect("expiresAt");

    let body = body_json(get(&router, BLUETOOTH_API, Some(&cookie)).await).await;
    assert_eq!(body["pairWindow"]["expiresAt"], json!(expires_at));
    assert_eq!(body["pending"][0]["passkey"], json!("042137"));

    let answered = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/pairing-requests/AA:BB:CC:DD:EE:02"),
        json!({ "accept": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(answered.status(), StatusCode::NO_CONTENT);

    let closed = json_request(
        &router,
        "DELETE",
        &window,
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(closed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.update_calls(),
        [
            "bluetooth window open",
            "bluetooth confirm AA:BB:CC:DD:EE:02 true",
            "bluetooth window close",
        ]
    );
    let refused = get(&router, &window, Some(&cookie)).await;
    assert_eq!(refused.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// The device path pairs, and serves no other verb; the console scans no
// longer, so there is no discovery route.
#[tokio::test]
pub(super) async fn the_device_path_pairs_and_serves_no_other_verb() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let paired = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/devices/AA:BB:CC:DD:EE:01/pair"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(paired.status(), StatusCode::NO_CONTENT);

    for path in ["devices/AA:BB:CC:DD:EE:01/confirm", "discovery"] {
        let response = json_request(
            &router,
            "POST",
            &format!("{BLUETOOTH_API}/{path}"),
            json!({ "accept": true, "on": true }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(fake.update_calls(), ["bluetooth pair AA:BB:CC:DD:EE:01"]);
}

// --- The access point and the scan ------------------------------------------
