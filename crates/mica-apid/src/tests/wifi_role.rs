//! The radio's role: one document and one atomic write for client or
//! access point.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::wifi_ap::ap_tree;
use super::*;

const WIFI_API: &str = "/api/v1/wifi";

// The access point as the unified write names it.
fn ap_config(country: &str, confirmed: Option<bool>) -> serde_json::Value {
    let mut ap = json!({
        "start": "always",
        "interface": "wlan0",
        "ssid": "mica-lab",
        "channel": 6,
        "countryCode": country,
        "address": "192.168.4.1/24",
        "holdDownSeconds": 120,
        "graceSeconds": 60,
        "security": "wpa2-wpa3",
    });
    if let Some(confirmed) = confirmed {
        ap["countryConfirmed"] = json!(confirmed);
    }
    ap
}

// The role reads from the two roles that can hold the radio: the access
// point's when it is on, then the station's.
#[tokio::test]
pub(super) async fn the_role_reads_from_the_access_point_then_the_station() {
    let mut tree = ap_tree();
    let (router, fake) = test_app(tree.clone());
    let (cookie, csrf) = mqtt_session(&router).await;
    let read = body_json(get(&router, WIFI_API, Some(&cookie)).await).await;
    assert_eq!(
        read,
        json!({ "enabled": true, "mode": "ap", "locked": false })
    );
    drop((router, fake, csrf));

    tree["wifi"]["ap"]["mode"] = json!("off");
    tree["wifi"]["client"]["enabled"] = json!(true);
    let (router, _fake) = test_app(tree.clone());
    let (cookie, _csrf) = mqtt_session(&router).await;
    let read = body_json(get(&router, WIFI_API, Some(&cookie)).await).await;
    assert_eq!(
        read,
        json!({ "enabled": true, "mode": "client", "locked": false })
    );

    tree["wifi"]["client"]["enabled"] = json!(false);
    let (router, _fake) = test_app(tree);
    let (cookie, _csrf) = mqtt_session(&router).await;
    let read = body_json(get(&router, WIFI_API, Some(&cookie)).await).await;
    assert_eq!(
        read,
        json!({ "enabled": false, "mode": "client", "locked": false })
    );
}

// Switching to the access point writes both roles and its configuration in
// one write; `start` is stored as its mode and the stored key is kept.
#[tokio::test]
pub(super) async fn switching_to_the_access_point_writes_both_roles_at_once() {
    let mut tree = ap_tree();
    tree["wifi"]["ap"]["mode"] = json!("off");
    tree["wifi"]["client"]["enabled"] = json!(true);
    let (router, fake) = test_app(tree);
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        WIFI_API,
        json!({ "enabled": true, "mode": "ap", "ap": ap_config("DE", None) }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), ["wifi.client.enabled", "wifi.ap"]);
    let wifi = fake.get_settings("wifi").await.unwrap();
    assert_eq!(wifi["client"]["enabled"], json!(false));
    assert_eq!(wifi["ap"]["mode"], json!("always"));
    assert_eq!(wifi["ap"]["security"], json!("wpa2-wpa3"));
    assert_eq!(wifi["ap"]["psk"], json!("labsecret1"));
    assert!(wifi["ap"].get("countryConfirmed").is_none(), "{wifi}");
}

// A new country is written only once the operator confirmed it.
#[tokio::test]
pub(super) async fn a_new_country_needs_confirming() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    for confirmed in [None, Some(false)] {
        let response = json_request(
            &router,
            "PUT",
            WIFI_API,
            json!({ "enabled": true, "mode": "ap", "ap": ap_config("FR", confirmed) }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(envelope(response).await["code"], "wifi_country_unconfirmed");
    }
    let response = json_request(
        &router,
        "PUT",
        "/api/v1/wifi/ap",
        {
            let mut ap = ap_config("FR", None);
            let start = ap.as_object_mut().unwrap().remove("start").unwrap();
            ap["mode"] = start;
            ap
        },
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "wifi_country_unconfirmed");
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());

    let response = json_request(
        &router,
        "PUT",
        WIFI_API,
        json!({ "enabled": true, "mode": "ap", "ap": ap_config("FR", Some(true)) }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.get_settings("wifi.ap.countryCode").await.unwrap(),
        json!("FR")
    );
}

// The station takes the radio's own addressing in the same write; off stops
// both roles.
#[tokio::test]
pub(super) async fn the_station_takes_the_radio_addressing_and_off_stops_both() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        WIFI_API,
        json!({ "enabled": true, "mode": "client", "client": { "addressing": { "dhcp": true, "dns": ["9.9.9.9"] } } }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.set_paths(),
        ["wifi.client.enabled", "wifi.ap.mode", "network.wlan0"]
    );
    assert_eq!(
        fake.get_settings("network.wlan0").await.unwrap(),
        json!({ "dhcp": true, "dns": ["9.9.9.9"] })
    );
    assert_eq!(
        fake.get_settings("wifi.ap.mode").await.unwrap(),
        json!("off")
    );

    let response = json_request(
        &router,
        "PUT",
        WIFI_API,
        json!({ "enabled": false, "mode": "client" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let wifi = fake.get_settings("wifi").await.unwrap();
    assert_eq!(wifi["client"]["enabled"], json!(false));
    assert_eq!(wifi["ap"]["mode"], json!("off"));
}

// What the write cannot mean is refused before anything is written.
#[tokio::test]
pub(super) async fn a_role_write_that_means_nothing_is_refused() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let mut sentinel = ap_config("DE", None);
    sentinel["psk"] = json!("<redacted>");
    for body in [
        json!({ "enabled": true, "mode": "mesh" }),
        json!({ "enabled": true, "mode": "ap" }),
        json!({ "enabled": true, "mode": "client", "ap": ap_config("DE", None) }),
        json!({ "enabled": false, "mode": "client", "client": { "addressing": { "dhcp": true } } }),
        json!({ "enabled": true, "mode": "ap", "ap": sentinel }),
        json!({ "enabled": true, "mode": "ap", "ap": { "start": "off" } }),
        json!({ "enabled": true, "mode": "client", "client": { "addressing": { "dhcp": false, "static": { "address": "10.0.0.5" } } } }),
        json!({ "enabled": true, "mode": "client", "extra": 1 }),
    ] {
        let response = json_request(
            &router,
            "PUT",
            WIFI_API,
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
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{body}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}
