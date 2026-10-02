//! Static routes and the DHCP server.

use crate::auth;
use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

// A tree with one statically addressed link to hang routes off.
pub(super) fn routed_tree() -> serde_json::Value {
    let hash = auth::hash_password("hunter2secret").unwrap();
    json!({
        "hostname": "mica",
        "network": {
            "eth1": { "dhcp": false, "static": { "address": "192.168.50.1/24", "dns": [] } },
        },
        "access": { "webAdmin": { "password_hash": hash } },
    })
}

// The accepted shape, written through the per-interface route and stored as
// sent: routes are a list and the server is one block.
#[tokio::test]
pub(super) async fn an_interface_takes_static_routes_and_a_dhcp_server() {
    let (router, fake) = test_app(routed_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        "/api/v1/network/eth1",
        json!({
            "dhcp": false,
            "static": { "address": "192.168.50.1/24", "dns": [] },
            "routes": [{ "destination": "10.20.0.0/16", "gateway": "192.168.50.254", "metric": 200 }],
            "dhcpServer": { "poolOffset": 100, "poolSize": 50, "dns": ["192.168.50.1"], "leaseSeconds": 3600 },
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let stored = fake.get_settings("network").await.unwrap();
    assert_eq!(
        stored["eth1"]["routes"][0]["destination"],
        json!("10.20.0.0/16")
    );
    assert_eq!(stored["eth1"]["dhcpServer"]["poolSize"], json!(50));
}

// The four refusals, each naming a configuration networkd would accept and
// nothing could use.
#[tokio::test]
pub(super) async fn routing_configurations_that_cannot_work_are_refused() {
    for entry in [
        // A destination that is not a network.
        json!({
            "dhcp": false,
            "static": { "address": "192.168.50.1/24", "dns": [] },
            "routes": [{ "destination": "not-a-network" }],
        }),
        // A next hop that is not an address.
        json!({
            "dhcp": false,
            "static": { "address": "192.168.50.1/24", "dns": [] },
            "routes": [{ "destination": "10.20.0.0/16", "gateway": "over-there" }],
        }),
        // Two default routes on one entry.
        json!({
            "dhcp": false,
            "static": { "address": "192.168.50.1/24", "gateway": "192.168.50.254", "dns": [] },
            "routes": [{ "destination": "0.0.0.0/0", "gateway": "192.168.50.1" }],
        }),
        // A server on a link that gets its own address from one.
        json!({
            "dhcp": true,
            "dhcpServer": { "poolOffset": 100, "poolSize": 50 },
        }),
        // A pool with no addresses in it.
        json!({
            "dhcp": false,
            "static": { "address": "192.168.50.1/24", "dns": [] },
            "dhcpServer": { "poolOffset": 100, "poolSize": 0 },
        }),
    ] {
        let (router, fake) = test_app(routed_tree());
        let (cookie, csrf) = mqtt_session(&router).await;

        let response = json_request(
            &router,
            "PUT",
            "/api/v1/network/eth1",
            entry.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{entry}"
        );
        assert_eq!(envelope(response).await["code"], "validation_failed");
        assert!(fake.set_paths().is_empty(), "a refused entry was written");
    }
}

// --- The WiFi station role and one known network -----------------------------
