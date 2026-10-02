//! The Wi-Fi station role and replacing a known network.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

pub(super) const WIFI_CLIENT_API: &str = "/api/v1/wifi/client";
pub(super) const WIFI_NETWORKS_API: &str = "/api/v1/wifi/client/networks";

// A device with a station on `wlan0` and one network stored with a key. The
// list is the existing [`wifi_tree`] helper's, so both suites read one shape.
pub(super) fn station_tree() -> serde_json::Value {
    wifi_tree(json!([
        { "ssid": "lab", "psk": "correct-horse", "hidden": false, "priority": 10 },
    ]))
}

// The station's radio is writable, and the known networks are not touched by
// the write that moves it.
#[tokio::test]
pub(super) async fn the_station_role_binds_a_radio_without_disturbing_the_known_networks() {
    let (router, fake) = test_app(station_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let read = body_json(get(&router, WIFI_CLIENT_API, Some(&cookie)).await).await;
    assert_eq!(read, json!({ "enabled": false, "interface": "wlan0" }));

    let response = json_request(
        &router,
        "PUT",
        WIFI_CLIENT_API,
        json!({ "enabled": true, "interface": "wlan1" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(
        fake.set_paths(),
        vec![
            "wifi.client.interface".to_string(),
            "wifi.client.enabled".to_string()
        ],
        "the station role is two scalar writes and never a subtree write"
    );
    let networks = fake.get_settings("wifi.client.networks").await.unwrap();
    assert_eq!(networks[0]["psk"], json!("correct-horse"));
}

#[tokio::test]
pub(super) async fn a_station_bound_to_something_that_is_not_an_interface_is_refused() {
    let (router, fake) = test_app(station_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        WIFI_CLIENT_API,
        json!({ "enabled": true, "interface": "not an interface" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(fake.set_paths().is_empty());
}

// The whole reason this route exists: an operator changing a priority has not
// been shown the key, so absence has to mean "keep it".
#[tokio::test]
pub(super) async fn replacing_a_network_without_a_key_keeps_the_stored_one() {
    let (router, fake) = test_app(station_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        &format!("{WIFI_NETWORKS_API}/lab"),
        json!({ "ssid": "lab", "hidden": true, "priority": 42 }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    // The echo redacts, exactly as the listing does.
    assert_eq!(body_json(response).await["psk"], json!("<redacted>"));
    let stored = fake.get_settings("wifi.client.networks").await.unwrap();
    assert_eq!(stored[0]["psk"], json!("correct-horse"));
    assert_eq!(stored[0]["priority"], json!(42));
    assert_eq!(stored[0]["hidden"], json!(true));
}

#[tokio::test]
pub(super) async fn replacing_a_network_with_a_key_takes_the_new_one() {
    let (router, fake) = test_app(station_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        &format!("{WIFI_NETWORKS_API}/lab"),
        json!({ "ssid": "lab", "psk": "another-secret", "hidden": false, "priority": 10 }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let stored = fake.get_settings("wifi.client.networks").await.unwrap();
    assert_eq!(stored[0]["psk"], json!("another-secret"));
}

// The three refusals: the sentinel written back, a rename through the wrong
// door, and an SSID that is not stored.
#[tokio::test]
pub(super) async fn a_replacement_is_refused_rather_than_guessed_at() {
    for (ssid, body, status) in [
        (
            "lab",
            json!({ "ssid": "lab", "psk": "<redacted>", "hidden": false, "priority": 1 }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "lab",
            json!({ "ssid": "renamed", "hidden": false, "priority": 1 }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "absent",
            json!({ "ssid": "absent", "hidden": false, "priority": 1 }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (router, fake) = test_app(station_tree());
        let (cookie, csrf) = mqtt_session(&router).await;

        let response = json_request(
            &router,
            "PUT",
            &format!("{WIFI_NETWORKS_API}/{ssid}"),
            body,
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(response.status(), status, "{ssid}");
        assert!(fake.set_paths().is_empty(), "a refused replacement wrote");
    }
}

// --- The MQTT resource ------------------------------------------------------
