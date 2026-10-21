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

fn radio(security: serde_json::Value, channels: serde_json::Value, acs: bool) -> serde_json::Value {
    json!({
        "capabilities": { "wifi": {
            "supported": true, "interfaces": ["wlan0"],
            "security": security, "bands": ["2.4"], "acs": acs,
            "apClientConcurrent": false, "channels": channels,
        } }
    })
}

fn ap_body(channel: u8, security: &str) -> serde_json::Value {
    json!({
        "mode": "always", "interface": "wlan0", "ssid": "mica-lab",
        "channel": channel, "countryCode": "DE", "address": "192.168.4.1/24",
        "holdDownSeconds": 120, "graceSeconds": 60, "security": security,
    })
}

// What the radio offers, and the fallback before it has said.
#[tokio::test]
pub(super) async fn the_capabilities_are_the_radio_s_or_the_baseline() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, _csrf) = mqtt_session(&router).await;
    let read = body_json(get(&router, "/api/v1/wifi/capabilities", Some(&cookie)).await).await;
    assert_eq!(
        read,
        json!({ "available": false, "security": ["wpa2-psk", "open"], "bands": ["2.4"], "acs": false, "apClientConcurrent": false })
    );
    fake.set_observed_network(radio(
        json!(["wpa2-psk", "wpa2-wpa3", "open"]),
        json!([1, 2, 3]),
        true,
    ));
    let read = body_json(get(&router, "/api/v1/wifi/capabilities", Some(&cookie)).await).await;
    assert_eq!(read["available"], true);
    assert_eq!(read["security"], json!(["wpa2-psk", "wpa2-wpa3", "open"]));
    assert_eq!(read["acs"], true);
}

// The country's channels that the radio has; a country that is not a code
// is refused.
#[tokio::test]
pub(super) async fn the_channels_are_the_country_s_that_the_radio_has() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, _csrf) = mqtt_session(&router).await;
    let read =
        body_json(get(&router, "/api/v1/wifi/channels?country=gr", Some(&cookie)).await).await;
    assert_eq!(read["countryCode"], "GR");
    assert_eq!(read["channels"], json!((1..=13).collect::<Vec<u8>>()));
    assert_eq!(read["auto"], false);
    let read =
        body_json(get(&router, "/api/v1/wifi/channels?country=US", Some(&cookie)).await).await;
    assert_eq!(read["channels"], json!((1..=11).collect::<Vec<u8>>()));

    fake.set_observed_network(radio(
        json!(["wpa2-psk", "open"]),
        json!([1, 6, 11, 12]),
        true,
    ));
    let read =
        body_json(get(&router, "/api/v1/wifi/channels?country=DE", Some(&cookie)).await).await;
    assert_eq!(read["channels"], json!([1, 6, 11, 12]));
    assert_eq!(read["auto"], true);

    for bad in ["/api/v1/wifi/channels", "/api/v1/wifi/channels?country=GRC"] {
        let response = get(&router, bad, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
}

// A channel the country does not allow is refused by its own code; 0 is
// the access point's own choice.
#[tokio::test]
pub(super) async fn a_channel_outside_the_country_s_is_refused() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let mut body = ap_body(14, "wpa2-psk");
    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        body.clone(),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "channel_not_allowed");
    assert_eq!(error["params"]["channel"], 14);
    body["channel"] = json!(0);
    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        body,
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.get_settings("wifi.ap").await.unwrap()["channel"], 0);
}

// WPA3 is refused on a radio that said it cannot run it, and taken on one
// that has not said.
#[tokio::test]
pub(super) async fn wpa3_is_refused_where_the_radio_lacks_it() {
    let (router, fake) = test_app(ap_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    fake.set_observed_network(radio(json!(["wpa2-psk", "open"]), json!([1, 6, 11]), false));
    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        ap_body(6, "wpa2-wpa3"),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "not_supported");
    assert_eq!(error["params"]["security"], "wpa2-wpa3");

    fake.set_observed_network(json!({ "capabilities": { "wifi": { "supported": true } } }));
    let response = json_request(
        &router,
        "PUT",
        WIFI_AP_API,
        ap_body(6, "wpa2-wpa3"),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
