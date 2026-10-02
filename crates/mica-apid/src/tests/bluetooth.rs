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
        "discoverable": false,
        "devices": { "AA:BB:CC:DD:EE:01": { "name": "phone", "trusted": true, "blocked": false } },
    });
    tree
}

// The read is micad's document: the two sides named apart, and the pairing
// code the console has to display.
#[tokio::test]
pub(super) async fn the_bluetooth_read_answers_micads_document() {
    let (router, _fake) = test_app(bluetooth_tree());
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, BLUETOOTH_API, Some(&cookie)).await).await;

    assert_eq!(body["adapter"]["available"], json!(true));
    assert_eq!(
        body["declared"]["AA:BB:CC:DD:EE:01"]["trusted"],
        json!(true)
    );
    assert_eq!(body["devices"]["entries"][0]["rssi"], json!(-55));
    assert_eq!(body["pin"], json!("4211"));
}

// The adapter write keeps the trust list: devices arrive by pairing, and a
// route that replaced the list would declare devices the adapter has no keys
// for.
#[tokio::test]
pub(super) async fn the_adapter_write_keeps_the_trust_list() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        BLUETOOTH_API,
        json!({ "enabled": true, "discoverable": true, "alias": "edge-42", "pin": "4211" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = fake.get_settings("bluetooth").await.unwrap();
    assert_eq!(stored["discoverable"], json!(true));
    assert_eq!(stored["alias"], json!("edge-42"));
    assert_eq!(stored["pin"], json!("4211"));
    assert_eq!(
        stored["devices"]["AA:BB:CC:DD:EE:01"]["name"],
        json!("phone"),
        "the trust list was dropped by an adapter write"
    );
}

// An absent alias or PIN takes the key out rather than writing an empty
// string: absent means "derive it", which is a different thing.
#[tokio::test]
pub(super) async fn an_absent_alias_or_code_is_removed_rather_than_emptied() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    json_request(
        &router,
        "PUT",
        BLUETOOTH_API,
        json!({ "enabled": true, "discoverable": false, "alias": "edge-42", "pin": "4211" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    json_request(
        &router,
        "PUT",
        BLUETOOTH_API,
        json!({ "enabled": true, "discoverable": false }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    let stored = fake.get_settings("bluetooth").await.unwrap();
    assert!(stored.get("alias").is_none(), "{stored}");
    assert!(stored.get("pin").is_none(), "{stored}");
}

// A pairing code no keypad could enter, and an alias no adapter could carry.
#[tokio::test]
pub(super) async fn an_adapter_setting_the_device_would_refuse_is_refused_here() {
    for body in [
        json!({ "enabled": true, "discoverable": false, "pin": "12" }),
        json!({ "enabled": true, "discoverable": false, "pin": "abcd" }),
        json!({ "enabled": true, "discoverable": false, "alias": "" }),
    ] {
        let (router, fake) = test_app(bluetooth_tree());
        let (cookie, csrf) = mqtt_session(&router).await;

        let response = json_request(
            &router,
            "PUT",
            BLUETOOTH_API,
            body.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        assert!(fake.set_paths().is_empty());
    }
}

// The two halves of one exchange, and nothing else on that path.
#[tokio::test]
pub(super) async fn the_device_path_pairs_confirms_and_serves_no_other_verb() {
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

    let confirmed = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/devices/AA:BB:CC:DD:EE:01/confirm"),
        json!({ "accept": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);

    let unknown = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/devices/AA:BB:CC:DD:EE:01/forget"),
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let calls = fake.update_calls();
    assert!(
        calls.contains(&"bluetooth pair AA:BB:CC:DD:EE:01".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"bluetooth confirm AA:BB:CC:DD:EE:01 true".to_string()),
        "{calls:?}"
    );
}

// Discovery is a POST with a body, and a GET on it is not served: nothing that
// follows a link should start a radio scan.
#[tokio::test]
pub(super) async fn discovery_is_a_post_and_never_a_navigation() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let refused = get(
        &router,
        &format!("{BLUETOOTH_API}/discovery"),
        Some(&cookie),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::METHOD_NOT_ALLOWED);

    let response = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/discovery"),
        json!({ "on": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        fake.update_calls()
            .contains(&"bluetooth discovery true".to_string()),
        "{:?}",
        fake.update_calls()
    );
}

// --- The access point and the scan ------------------------------------------
