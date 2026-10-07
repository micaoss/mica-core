//! The API schemas against the settings model.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

// The documented container schema against `micad_settings::ContainerUnit`,
// field for field, on the same rule the interface mirror takes: the model is
// what the body deserializes into, so a field added to it cannot go
// undocumented here.
#[test]
pub(super) fn the_container_schema_matches_the_settings_model() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let unit = micad_settings::ContainerUnit {
        image: "docker.io/library/busybox:1".to_string(),
        command: vec!["sleep".to_string(), "infinity".to_string()],
        environment: std::collections::BTreeMap::from([("TZ".to_string(), "UTC".to_string())]),
        publish: vec![micad_settings::PublishedPort {
            host: 8080,
            container: 80,
            protocol: micad_settings::PortProtocol::Tcp,
        }],
        volumes: vec![micad_settings::VolumeMount {
            host: "/mica/apps/app".to_string(),
            container: "/data".to_string(),
            read_only: true,
        }],
        restart: micad_settings::RestartPolicy::Always,
        auto_start: true,
        pids: Some(128),
        memory: Some("256m".to_string()),
        cpu: Some("0.5".to_string()),
    };

    for (schema, model) in [
        ("ContainerDeclaration", serde_json::to_value(&unit).unwrap()),
        (
            "ContainerPort",
            serde_json::to_value(&unit.publish[0]).unwrap(),
        ),
        (
            "ContainerVolume",
            serde_json::to_value(&unit.volumes[0]).unwrap(),
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
        routes: vec![micad_settings::RouteConfig {
            destination: "10.20.0.0/16".to_string(),
            gateway: Some("10.8.0.1".to_string()),
            metric: Some(200),
        }],
        dhcp_server: Some(micad_settings::DhcpServerConfig {
            pool_offset: 100,
            pool_size: 50,
            dns: vec!["10.8.0.1".to_string()],
            lease_seconds: Some(3600),
        }),
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
            "StaticRoute",
            serde_json::to_value(&iface.routes[0]).unwrap(),
        ),
        (
            "DhcpServer",
            serde_json::to_value(iface.dhcp_server.clone().unwrap()).unwrap(),
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
