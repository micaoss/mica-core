//! The SSDP read and the UPnP device description.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

use super::*;
use crate::routes::redirect_app;

const SSDP_PATH: &str = "/api/v1/ssdp";
const DESCRIPTION: &str = "/ssdp/device.xml";

// The live state micad's ssdp reconciler publishes while it runs.
fn ssdp_state(https: bool) -> serde_json::Value {
    json!({
        "enabled": true,
        "source": "product",
        "active": true,
        "udn": "uuid:a8d31972-5f0f-8c1e-ae28-68c78f6b4568",
        "serialNumber": "3f1c0a9e5d2b7c84",
        "friendlyName": "mica-ab12",
        "manufacturer": "Mica",
        "modelName": "cx3576.full",
        "deviceType": "urn:micaos:device:mica:1",
        "console": if https { json!({ "scheme": "https", "port": 9443 }) } else { json!({ "scheme": "http", "port": 8080 }) },
    })
}

async fn description(router: &Router, host: Option<&str>) -> axum::response::Response {
    let mut request = Request::builder().uri(DESCRIPTION);
    if let Some(host) = host {
        request = request.header("host", host);
    }
    send(router, request.body(Body::empty()).unwrap()).await
}

#[tokio::test]
pub(super) async fn the_ssdp_read_answers_the_published_state() {
    let (router, fake) = test_app(mqtt_tree(false));
    fake.set_state_entry("ssdp", ssdp_state(false));
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, SSDP_PATH, Some(&cookie)).await).await;

    assert_eq!(body["available"], json!(true));
    assert_eq!(body["state"]["active"], json!(true));
    assert_eq!(body["state"]["friendlyName"], json!("mica-ab12"));
    assert_eq!(body["state"]["console"]["port"], json!(8080));

    assert_eq!(
        get(&router, SSDP_PATH, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// The description needs no credential and names the device, never the
// machine id or the device code, and the console on the address asked.
#[tokio::test]
pub(super) async fn the_description_names_the_device_and_its_console() {
    let (router, fake) = test_app(mqtt_tree(false));
    fake.set_state_entry("ssdp", ssdp_state(true));

    let response = description(&router, Some("192.168.1.24:8080")).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(header_value(&response, axum::http::header::CONTENT_TYPE).starts_with("text/xml"));
    let xml = body_string(response).await;
    for part in [
        "<root xmlns=\"urn:schemas-upnp-org:device-1-0\" configId=\"1\">",
        "<deviceType>urn:micaos:device:mica:1</deviceType>",
        "<friendlyName>mica-ab12</friendlyName>",
        "<manufacturer>Mica</manufacturer>",
        "<modelName>cx3576.full</modelName>",
        "<serialNumber>3f1c0a9e5d2b7c84</serialNumber>",
        "<UDN>uuid:a8d31972-5f0f-8c1e-ae28-68c78f6b4568</UDN>",
        "<presentationURL>https://192.168.1.24:9443/</presentationURL>",
    ] {
        assert!(xml.contains(part), "{part}: {xml}");
    }
    assert!(!xml.contains("serviceList"), "no services block: {xml}");

    // A host that is not one is not echoed.
    let xml = body_string(description(&router, Some("<evil>")).await).await;
    assert!(!xml.contains("presentationURL"), "{xml}");
}

// Off, or with the console off the network, there is nothing to describe.
#[tokio::test]
pub(super) async fn the_description_is_404_while_the_responder_does_not_run() {
    let (router, fake) = test_app(mqtt_tree(false));
    assert_eq!(
        description(&router, None).await.status(),
        StatusCode::NOT_FOUND
    );

    let mut held = ssdp_state(false);
    held["active"] = json!(false);
    held["reason"] = json!("the console is not on the network");
    held.as_object_mut().unwrap().remove("udn");
    held.as_object_mut().unwrap().remove("serialNumber");
    fake.set_state_entry("ssdp", held);
    assert_eq!(
        description(&router, Some("192.168.1.24")).await.status(),
        StatusCode::NOT_FOUND
    );
}

// With HTTPS on, the HTTP listener serves the description, where LOCATION
// points, and redirects every other request.
#[tokio::test]
pub(super) async fn the_http_listener_serves_the_description_and_redirects_the_rest() {
    let fake = std::sync::Arc::new(crate::settings_api::FakeSettings::new(mqtt_tree(false)));
    fake.set_state_entry("ssdp", ssdp_state(true));
    let router = redirect_app(9443, crate::routes::AppState::new(fake, SIGNING_KEY));

    let response = description(&router, Some("192.168.1.24:8080")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_string(response).await.contains("<UDN>"));

    let response = send(
        &router,
        Request::builder()
            .uri("/api/v1/meta")
            .header("host", "192.168.1.24:8080")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        header_value(&response, axum::http::header::LOCATION),
        "https://192.168.1.24:9443/api/v1/meta"
    );
}
