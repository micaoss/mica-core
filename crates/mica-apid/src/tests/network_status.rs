//! The observed network joined with the request and the configured map.

use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use serde_json::json;

use super::*;

fn observed() -> serde_json::Value {
    json!({
        "interfaces": { "available": true, "count": 2, "entries": [
            { "name": "eth0", "present": true, "addresses": [{ "family": "ipv4", "address": "192.168.1.24", "prefixLength": 24 }] },
            { "name": "wg0", "present": true, "addresses": [], "wireguard": { "available": true, "peers": [
                { "publicKey": PEER_KEY, "latestHandshake": 1_792_000_000u64, "rxBytes": 4096, "txBytes": 2048, "persistentKeepalive": 0 },
                { "publicKey": OTHER_PEER_KEY, "latestHandshake": null, "rxBytes": 0, "txBytes": 0, "persistentKeepalive": 0 }
            ] } }
        ] },
        "defaultRoutes": { "available": true, "count": 0, "entries": [] },
    })
}

async fn status_with_local(router: &Router, token: &str, local: Option<&str>) -> serde_json::Value {
    let mut request = Request::builder()
        .uri("/api/v1/network/status")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    if let Some(local) = local {
        request
            .extensions_mut()
            .insert(crate::local_addr::LocalAddr(local.parse().unwrap()));
    }
    let response = send(router, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

// The request's own address names the interface it came in on; an address
// no interface holds names none.
#[tokio::test]
pub(super) async fn the_session_names_the_interface_the_request_arrived_on() {
    let (router, fake, _cookie, token) = kinds_app().await;
    fake.set_observed_network(observed());
    let status = status_with_local(&router, &token, Some("[::ffff:192.168.1.24]:443")).await;
    assert_eq!(
        status["session"],
        json!({ "interface": "eth0", "localAddress": "192.168.1.24" })
    );
    let status = status_with_local(&router, &token, Some("10.9.9.9:443")).await;
    assert_eq!(
        status["session"],
        json!({ "interface": null, "localAddress": "10.9.9.9" })
    );
    let status = status_with_local(&router, &token, None).await;
    assert_eq!(
        status["session"],
        json!({ "interface": null, "localAddress": null })
    );
}

// A configured interface the device does not have is listed as missing;
// with no interface list, nothing is.
#[tokio::test]
pub(super) async fn a_configured_interface_the_device_lacks_reads_as_not_present() {
    let (router, fake, _cookie, token) = kinds_app().await;
    fake.set_observed_network(observed());
    let status = status_with_local(&router, &token, None).await;
    let missing: Vec<_> = status["interfaces"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["present"] == false)
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(missing, ["br0", "eth0.100", "eth1"]);

    fake.set_observed_network(json!({ "interfaces": { "available": false, "detail": "fake" } }));
    let status = status_with_local(&router, &token, None).await;
    assert!(status["interfaces"].get("entries").is_none(), "{status}");
}

// The peer collection carries what the tunnel reports, joined by key.
#[tokio::test]
pub(super) async fn listed_peers_carry_their_handshake_and_traffic() {
    let (router, fake, _cookie, token) = kinds_app().await;
    fake.set_observed_network(observed());
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["latestHandshake"], 1_792_000_000u64);
    assert_eq!(listed[0]["rxBytes"], 4096);
    assert_eq!(listed[0]["txBytes"], 2048);

    fake.set_observed_network(json!({ "interfaces": { "available": false, "detail": "fake" } }));
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["latestHandshake"], serde_json::Value::Null);
    assert_eq!(listed[0]["rxBytes"], serde_json::Value::Null);
}

// Where a peer stands and whether its routes leave by the tunnel, from the
// kernel's endpoint and per-prefix route check; null when the tunnel could
// not be asked.
#[tokio::test]
pub(super) async fn listed_peers_carry_their_state_and_routes() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let observed = |endpoint: serde_json::Value, routed: bool| {
        json!({
            "interfaces": { "available": true, "count": 1, "entries": [
                { "name": "wg0", "present": true, "addresses": [], "wireguard": { "available": true, "peers": [
                    { "publicKey": PEER_KEY, "latestHandshake": null, "rxBytes": 0, "txBytes": 0,
                      "persistentKeepalive": 0, "endpoint": endpoint,
                      "allowedIps": [{ "prefix": "10.8.0.0/24", "routed": routed }] }
                ] } }
            ] },
        })
    };

    // The stored peer names a host endpoint the kernel has not resolved.
    fake.set_observed_network(observed(serde_json::Value::Null, true));
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["operationalState"], "pending_endpoint_resolution");
    assert_eq!(listed[0]["routesReady"], true);
    assert_eq!(listed[0]["unroutedIps"], json!([]));

    fake.set_observed_network(observed(json!("10.8.0.9:51820"), false));
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["operationalState"], "endpoint_route_loop");
    assert_eq!(listed[0]["routesReady"], false);
    assert_eq!(listed[0]["unroutedIps"], json!(["10.8.0.0/24"]));

    fake.set_observed_network(observed(json!("198.51.100.4:51820"), true));
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["operationalState"], "ready");

    fake.set_observed_network(json!({ "interfaces": { "available": false, "detail": "fake" } }));
    let listed = body_json(bearer(&router, "GET", &peers_url("wg0"), &token).await).await;
    assert_eq!(listed[0]["operationalState"], serde_json::Value::Null);
    assert_eq!(listed[0]["routesReady"], serde_json::Value::Null);
}
