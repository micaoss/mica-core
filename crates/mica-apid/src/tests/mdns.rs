//! The mDNS read.

use serde_json::json;

use super::*;

const MDNS_PATH: &str = "/api/v1/mdns";

// The live state micad's mdns reconciler publishes, with one service from a
// drop-in and one refused file.
fn mdns_state(enabled: bool) -> serde_json::Value {
    json!({
        "enabled": enabled,
        "source": "setting",
        "hostname": "mica.local",
        "services": [
            {
                "instance": "mica", "type": "_mica._tcp", "port": 8080,
                "txt": { "txtvers": "1", "hostname": "mica" }, "addresses": [],
                "source": "built-in", "published": enabled,
            },
            {
                "instance": "Grafana", "type": "_http._tcp", "port": 3000,
                "txt": {}, "addresses": [], "source": "image:grafana", "published": enabled,
            },
        ],
        "refused": [{ "source": "runtime:broken", "error": "EOF while parsing" }],
    })
}

#[tokio::test]
pub(super) async fn the_mdns_read_answers_the_published_state() {
    let (router, fake) = test_app(mqtt_tree(false));
    fake.set_state_entry("mdns", mdns_state(true));
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, MDNS_PATH, Some(&cookie)).await).await;

    assert_eq!(body["available"], json!(true));
    assert_eq!(body["state"]["hostname"], json!("mica.local"));
    assert_eq!(body["state"]["services"][1]["type"], json!("_http._tcp"));
    assert_eq!(
        body["state"]["services"][1]["source"],
        json!("image:grafana")
    );
    assert_eq!(
        body["state"]["refused"][0]["source"],
        json!("runtime:broken")
    );
}

// No state, or a failed apply, is said as such and never as a responder at
// its defaults.
#[tokio::test]
pub(super) async fn the_mdns_read_says_why_there_is_no_state() {
    let (router, fake) = test_app(mqtt_tree(false));
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, MDNS_PATH, Some(&cookie)).await).await;
    assert_eq!(body["available"], json!(false));
    assert!(body.get("state").is_none(), "{body}");
    assert!(body["error"].is_string(), "{body}");

    fake.set_state_entry(
        "mdns",
        json!({ "error": "start the mDNS responder: no interface" }),
    );
    let body = body_json(get(&router, MDNS_PATH, Some(&cookie)).await).await;
    assert_eq!(body["available"], json!(false));
    assert_eq!(
        body["error"],
        json!("start the mDNS responder: no interface")
    );
}

#[tokio::test]
pub(super) async fn the_mdns_read_needs_a_credential() {
    let (router, fake) = test_app(mqtt_tree(false));
    fake.set_state_entry("mdns", mdns_state(false));

    let response = get(&router, MDNS_PATH, None).await;

    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
}
