//! The access point routes and the Wi-Fi scan.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

pub(super) const WIFI_AP_API: &str = "/api/v1/wifi/ap";

// A device with an access point configured and keyed.
pub(super) fn ap_tree() -> serde_json::Value {
    let mut tree = wifi_tree(json!([]));
    tree["wifi"]["ap"] = json!({
        "mode": "provisioning",
        "interface": "wlan0",
        "ssid": "mica-lab",
        "psk": "labsecret1",
        "channel": 11,
        "countryCode": "DE",
        "address": "192.168.4.1/24",
        "holdDownSeconds": 120,
        "graceSeconds": 60,
    });
    tree
}

// The read never carries the key, and the write keeps it when the body sends
// none -- which is every write by an operator who was shown the redaction.
#[tokio::test]
pub(super) async fn the_access_point_reads_redacted_and_keeps_its_key_through_a_write() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let read = body_json(get(&router, WIFI_AP_API, Some(&cookie)).await).await;
    assert_eq!(read["psk"], json!("<redacted>"));
    assert_eq!(read["mode"], json!("provisioning"));

    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        json!({
            "mode": "always",
            "interface": "wlan0",
            "ssid": "mica-lab",
            "channel": 6,
            "countryCode": "DE",
            "address": "192.168.4.1/24",
            "holdDownSeconds": 120,
            "graceSeconds": 60,
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = fake.get_settings("wifi.ap").await.unwrap();
    assert_eq!(stored["mode"], json!("always"));
    assert_eq!(stored["channel"], json!(6));
    assert_eq!(
        stored["psk"],
        json!("labsecret1"),
        "the stored key was dropped"
    );
}

// Security, pool and lease are stored as sent, the pool as an offset and a
// count, and read back as addresses.
#[tokio::test]
pub(super) async fn the_access_point_takes_its_security_pool_and_lease() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        json!({
            "mode": "always",
            "interface": "wlan0",
            "channel": 6,
            "countryCode": "DE",
            "address": "192.168.4.1/24",
            "holdDownSeconds": 120,
            "graceSeconds": 60,
            "security": "wpa2-wpa3",
            "dhcpServer": { "poolStart": "192.168.4.100", "poolEnd": "192.168.4.149", "leaseSeconds": 3600 },
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = fake.get_settings("wifi.ap").await.unwrap();
    assert_eq!(stored["security"], json!("wpa2-wpa3"));
    assert_eq!(stored["pool"], json!({ "offset": 100, "size": 50 }));
    assert_eq!(stored["leaseSeconds"], json!(3600));
    let read = body_json(get(&router, WIFI_AP_API, Some(&cookie)).await).await;
    assert_eq!(
        read["dhcpServer"],
        json!({ "poolStart": "192.168.4.100", "poolEnd": "192.168.4.149", "leaseSeconds": 3600 })
    );
    assert!(read.get("pool").is_none(), "{read}");
}

// With no pool stored, the read names the one micad serves, and writing that
// pool back stores none, so it keeps following the address.
#[tokio::test]
pub(super) async fn the_derived_pool_reads_as_addresses_and_stays_derived() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let mut read = body_json(get(&router, WIFI_AP_API, Some(&cookie)).await).await;
    assert_eq!(
        read["dhcpServer"],
        json!({ "poolStart": "192.168.4.2", "poolEnd": "192.168.4.254" })
    );

    read.as_object_mut().unwrap().remove("psk");
    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        read,
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = fake.get_settings("wifi.ap").await.unwrap();
    assert!(stored.get("pool").is_none(), "{stored}");
    assert!(stored.get("dhcpServer").is_none(), "{stored}");
}

// A pool the DHCP server cannot serve is refused by what is wrong with it.
#[tokio::test]
pub(super) async fn a_pool_is_refused_by_its_own_code() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    for (start, end, code) in [
        ("192.168.5.10", "192.168.5.20", "pool_out_of_subnet"),
        ("192.168.4.10", "192.168.4.255", "pool_out_of_subnet"),
        ("192.168.4.20", "192.168.4.10", "pool_order"),
        ("192.168.4.1", "192.168.4.10", "pool_contains_address"),
        ("192.168.4.x", "192.168.4.10", "validation_failed"),
    ] {
        let body = json!({
            "mode": "always",
            "interface": "wlan0",
            "channel": 11,
            "countryCode": "DE",
            "address": "192.168.4.1/24",
            "holdDownSeconds": 120,
            "graceSeconds": 60,
            "dhcpServer": { "poolStart": start, "poolEnd": end },
        });
        let response = json_request(
            &router,
            "PUT",
            WIFI_AP_API,
            body,
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{start}-{end}"
        );
        assert_eq!(envelope(response).await["code"], code, "{start}-{end}");
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The refusals: a mode that is not one of the three, an interface that is not
// one, an address that is not a network, and the sentinel written back.
#[tokio::test]
pub(super) async fn an_access_point_the_device_would_refuse_is_refused_here() {
    let base = json!({
        "mode": "always",
        "interface": "wlan0",
        "channel": 6,
        "countryCode": "DE",
        "address": "192.168.4.1/24",
        "holdDownSeconds": 120,
        "graceSeconds": 60,
    });
    for change in [
        json!({ "mode": "sometimes" }),
        json!({ "interface": "not an interface" }),
        json!({ "address": "192.168.4.1" }),
        json!({ "psk": "<redacted>" }),
        json!({ "psk": "short" }),
        json!({ "security": "wep" }),
        json!({ "pool": { "offset": 100, "size": 10 } }),
        json!({ "dhcpServer": { "poolStart": "192.168.4.100", "poolEnd": "192.168.4.149", "leaseSeconds": 30 } }),
    ] {
        let (router, fake) = test_app(ap_tree());
        let (cookie, csrf) = mqtt_session(&router).await;
        let mut body = base.clone();
        for (key, value) in change.as_object().unwrap() {
            body[key] = value.clone();
        }

        let response = json_request(
            &router,
            "PUT",
            WIFI_AP_API,
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
        assert!(
            fake.set_paths().is_empty(),
            "a refused access point was written"
        );
    }
}

// The scan is POST-only -- it puts the radio to work -- and answers micad's
// own document.
#[tokio::test]
pub(super) async fn the_scan_is_a_post_that_answers_what_the_radio_found() {
    let (router, _fake) = test_app(station_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let refused = get(&router, "/api/v1/wifi/client/scan", Some(&cookie)).await;
    assert_eq!(refused.status(), StatusCode::METHOD_NOT_ALLOWED);

    let response = json_request(
        &router,
        "POST",
        "/api/v1/wifi/client/scan",
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["available"], json!(true));
    assert_eq!(body["networks"][0]["ssid"], json!("workshop"));
}

// --- Declared containers -----------------------------------------------------
