//! The per-rule codes a refused network write answers, and its `details`.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

async fn refused(router: &Router, token: &str, map: serde_json::Value) -> serde_json::Value {
    let response = put_network_map(router, token, &map).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    envelope(response).await
}

fn codes(error: &serde_json::Value) -> Vec<(String, String, String)> {
    error["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|detail| {
            (
                detail["interface"].as_str().unwrap().to_string(),
                detail["field"].as_str().unwrap().to_string(),
                detail["code"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn static_at(address: &str) -> serde_json::Value {
    json!({ "dhcp": false, "static": { "address": address } })
}

// Each rule a write is held to answers its own code, on the entry and the
// field it is about, with the values its message names.
#[tokio::test]
pub(super) async fn each_rule_answers_its_own_code() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let error = refused(
        &router,
        &token,
        json!({
            "eth0": static_at("192.168.10.2/24"),
            "eth1": static_at("192.168.10.9/16"),
        }),
    )
    .await;
    assert_eq!(error["code"], "address_overlap");
    assert_eq!(error["params"], json!({ "name": "eth0" }));
    assert_eq!(
        codes(&error),
        [(
            "eth1".into(),
            "static.address".into(),
            "address_overlap".into()
        )]
    );

    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": false, "static": { "address": "192.168.10.2/24", "gateway": "192.168.11.1" } },
    }))
    .await;
    assert_eq!(error["code"], "gateway_out_of_subnet");
    assert_eq!(error["details"][0]["field"], "static.gateway");

    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": true, "routes": [{ "destination": "0.0.0.0/0", "gateway": "192.168.10.1", "metric": 100 }] },
        "eth1": { "dhcp": true, "routes": [{ "destination": "0.0.0.0/0", "gateway": "10.0.0.1", "metric": 100 }] },
    }))
    .await;
    assert_eq!(error["code"], "metric_duplicate");
    assert_eq!(error["params"], json!({ "name": "eth0", "metric": 100 }));
    assert_eq!(error["details"][0]["interface"], "eth1");

    let vlan =
        |id: u16| json!({ "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth0", "id": id } });
    let error = refused(
        &router,
        &token,
        json!({
            "eth0": { "dhcp": true },
            "eth0.100": vlan(100),
            "lan100": vlan(100),
        }),
    )
    .await;
    assert_eq!(error["code"], "vlan_duplicate");
    assert_eq!(
        error["params"],
        json!({ "parent": "eth0", "id": 100, "name": "eth0.100" })
    );
    assert_eq!(error["details"][0]["interface"], "lan100");

    let bridge =
        |port: &str| json!({ "kind": "bridge", "dhcp": true, "bridge": { "ports": [port] } });
    let error = refused(
        &router,
        &token,
        json!({
            "eth1": { "dhcp": false },
            "br0": bridge("eth1"),
            "br1": bridge("eth1"),
        }),
    )
    .await;
    assert_eq!(error["code"], "bridge_port_taken");
    assert_eq!(error["params"], json!({ "port": "eth1", "bridge": "br0" }));

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Every rule the body breaks is in `details`, not only the first.
#[tokio::test]
pub(super) async fn every_broken_rule_is_listed() {
    let (router, _fake, _cookie, token) = kinds_app().await;
    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": false, "static": { "address": "192.168.10.2/24", "gateway": "10.9.9.9" } },
        "eth1": static_at("192.168.10.3/24"),
        "vlan9": { "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth7", "id": 9 } },
    }))
    .await;
    assert_eq!(
        codes(&error),
        [
            (
                "vlan9".into(),
                "vlan.parent".into(),
                "vlan_parent_invalid".into()
            ),
            (
                "eth0".into(),
                "static.gateway".into(),
                "gateway_out_of_subnet".into()
            ),
            (
                "eth1".into(),
                "static.address".into(),
                "address_overlap".into()
            ),
        ]
    );
    assert_eq!(error["code"], "vlan_parent_invalid");
}

// A rule two stored entries already break is not this request's fault: an
// edit of a third entry goes through.
#[tokio::test]
pub(super) async fn a_collision_between_entries_the_write_does_not_touch_is_not_its_fault() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let mut map = stored_network_map(&fake).await;
    map["eth0"] = static_at("192.168.10.2/24");
    map["eth1"] = static_at("192.168.10.3/24");
    map.as_object_mut().unwrap().remove("br0");
    fake.set_settings("network", &map).await.unwrap();

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("wg0"),
        &token,
        &json!({ "kind": "wireguard", "dhcp": false, "static": { "address": "10.8.0.2/24" }, "wireguard": { "peers": [] } }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth1"),
        &token,
        &static_at("192.168.10.3/24").to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "address_overlap");
}
