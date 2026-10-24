//! The API schemas against the settings model.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

// The documented interface schema against `micad_settings::IfaceSettings`
// itself, field for field, so a field added to the model cannot go
// undocumented here.
#[test]
pub(super) fn the_network_schema_matches_the_settings_model() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let peer = micad_settings::WireguardPeer {
        public_key: PEER_KEY.to_string(),
        allowed_ips: vec!["10.8.0.0/24".to_string()],
        endpoint: Some("vpn.example.net:51820".to_string()),
        persistent_keepalive: Some(25),
        name: Some("laptop".to_string()),
        preshared_key: Some(PEER_KEY.to_string()),
    };
    let iface = micad_settings::IfaceSettings {
        kind: micad_settings::IfaceKind::Wireguard,
        dhcp: false,
        static_: Some(micad_settings::StaticConfig {
            address: "10.8.0.2/24".to_string(),
            gateway: Some("10.8.0.1".to_string()),
            dns: vec!["1.1.1.1".to_string()],
        }),
        dns: vec!["9.9.9.9".to_string()],
        vlan: Some(micad_settings::VlanConfig {
            parent: "eth0".to_string(),
            id: 100,
        }),
        bridge: Some(micad_settings::BridgeConfig {
            ports: vec!["eth1".to_string()],
        }),
        wireguard: Some(micad_settings::WireguardConfig {
            listen_port: Some(51820),
            peers: vec![peer.clone()],
        }),
        can: Some(micad_settings::CanConfig {
            bitrate: 500_000,
            restart_ms: Some(100),
            fd: Some(true),
            data_bitrate: Some(2_000_000),
        }),
        routes: vec![micad_settings::RouteConfig {
            destination: "10.20.0.0/16".to_string(),
            gateway: Some("10.8.0.1".to_string()),
            metric: Some(200),
        }],
        internet: Some(false),
    };

    for (schema, model) in [
        ("NetworkInterface", serde_json::to_value(&iface).unwrap()),
        (
            "StaticAddressing",
            serde_json::to_value(iface.static_.clone().unwrap()).unwrap(),
        ),
        (
            "VlanParameters",
            serde_json::to_value(iface.vlan.clone().unwrap()).unwrap(),
        ),
        (
            "BridgeParameters",
            serde_json::to_value(iface.bridge.clone().unwrap()).unwrap(),
        ),
        (
            "WireguardParameters",
            serde_json::to_value(iface.wireguard.clone().unwrap()).unwrap(),
        ),
        ("WireguardPeerEntry", serde_json::to_value(&peer).unwrap()),
        (
            "CanParameters",
            serde_json::to_value(iface.can.clone().unwrap()).unwrap(),
        ),
        (
            "StaticRoute",
            serde_json::to_value(&iface.routes[0]).unwrap(),
        ),
    ] {
        let mut documented: Vec<String> = document["components"]["schemas"][schema]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{schema} is an object schema"))
            .keys()
            .cloned()
            .collect();
        let mut fields: Vec<String> = model
            .as_object()
            .unwrap_or_else(|| panic!("{schema}'s model is an object"))
            .keys()
            .cloned()
            .collect();
        documented.sort();
        fields.sort();
        assert_eq!(
            documented, fields,
            "the documented {schema} has drifted from the settings model"
        );
    }
}

// The collection identifier contract's third clause, held across **every**
// collection at once: a duplicate is **409**, with a per-collection code.
#[tokio::test]
pub(super) async fn every_collection_answers_409_for_a_duplicate() {
    let mut tree = kinds_tree("hunter2secret");
    tree["access"]["ssh"] =
        ssh_tree(json!([stored_key(REAL_ED25519_LINE)]))["access"]["ssh"].clone();
    tree["wifi"] = wifi_tree(json!([
        { "ssid": "roastery", "psk": "hunter2hunter2", "hidden": false, "priority": 0 },
    ]))["wifi"]
        .clone();
    let (tree, token) = with_token(tree);
    let (router, fake) = test_app(tree);

    for (path, body, code) in [
        (
            "/api/v1/ssh/authorized-keys",
            json!({ "key": format!("{} relabelled", canonical(REAL_ED25519_LINE)) }),
            "key_exists",
        ),
        (
            "/api/v1/wifi/client/networks",
            json!({ "ssid": "roastery", "psk": "adifferentkey" }),
            "ssid_exists",
        ),
        (
            "/api/v1/network/wg0/peers",
            json!({ "publicKey": PEER_KEY }),
            "peer_exists",
        ),
    ] {
        let response = bearer_json(&router, "POST", path, &token, &body.to_string()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
        assert_api_headers(&response, path);
        let error = envelope(response).await;
        assert_eq!(error["code"], code, "{path}");
        assert_eq!(error["source"], "apid", "{path}");
    }
    // Not one of them wrote: a refused duplicate leaves the collection alone.
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The three actions. No state to `GET` and no idempotency to
// promise, so every assertion below is about the status code, the call that
// did or did not reach micad, and what the response body does not contain.
