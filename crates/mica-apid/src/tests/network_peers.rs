//! WireGuard peers.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use serde_json::json;

use super::*;

// The peer collection end to end: list, add, remove.
#[tokio::test]
pub(super) async fn the_peer_collection_lists_adds_and_removes() {
    let (router, fake, _cookie, token) = kinds_app().await;

    let response = bearer(&router, "GET", &peers_url("wg0"), &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "peer listing");
    let listed: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["publicKey"], json!(PEER_KEY));
    assert_eq!(listed[0]["allowedIps"], json!(["10.8.0.0/24"]));

    let response = bearer_json(
        &router,
        "POST",
        &peers_url("wg0"),
        &token,
        &json!({
            "publicKey": OTHER_PEER_KEY,
            "allowedIps": ["10.8.1.0/24"],
            "endpoint": "vpn2.example.net:51820",
            "persistentKeepalive": 25,
        })
        .to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let echoed: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(echoed["publicKey"], json!(OTHER_PEER_KEY));
    assert_eq!(echoed["persistentKeepalive"], json!(25));
    // Only the peer list was written, not the whole entry.
    assert_eq!(
        fake.set_paths(),
        vec!["network.wg0.wireguard.peers".to_string()]
    );

    let response = bearer(&router, "DELETE", &peer_url("wg0", OTHER_PEER_KEY), &token).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let peers = fake
        .get_settings("network.wg0.wireguard.peers")
        .await
        .unwrap();
    assert_eq!(peers.as_array().unwrap().len(), 1, "{peers}");
    assert_eq!(peers[0]["publicKey"], json!(PEER_KEY));
}

// the sweep, discharged: the typed route
// answers **404 before anything is written** for the interface the pane
// silently creates a broken entry for.
#[tokio::test]
pub(super) async fn the_api_peer_add_refuses_an_undeclared_interface_where_the_pane_writes_one() {
    let (router, fake, _cookie, token) = kinds_app().await;

    let response = bearer_json(
        &router,
        "POST",
        &peers_url("wg9"),
        &token,
        &json!({ "publicKey": PEER_KEY }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_api_headers(&response, "peer add on an undeclared interface");
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["path"], json!(NETWORK_DOT_PATH));
    // Nothing was written, which is the half the pane gets wrong: no write at
    // all, and therefore no `network.wg9` of the default kind.
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert!(
        stored_network_map(&fake).await.get("wg9").is_none(),
        "an undeclared interface was created"
    );

    // The same 404 on the other two operations of the collection.
    assert_eq!(
        bearer(&router, "GET", &peers_url("wg9"), &token)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        bearer(&router, "DELETE", &peer_url("wg9", PEER_KEY), &token)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// A declared entry of the wrong kind is **422** and not 404, which is the
// same split micad's rotate-key makes: the URL names a real entry, and
// what is wrong is the argument.
#[tokio::test]
pub(super) async fn peers_on_an_interface_that_is_not_a_tunnel_are_422() {
    let (router, fake, _cookie, token) = kinds_app().await;

    for (method, path) in [
        ("GET", peers_url("eth0")),
        ("DELETE", peer_url("eth0", PEER_KEY)),
    ] {
        let response = bearer(&router, method, &path, &token).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed", "{path}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("is not a WireGuard interface"),
            "{error}"
        );
    }

    let response = bearer_json(
        &router,
        "POST",
        &peers_url("eth0"),
        &token,
        &json!({ "publicKey": PEER_KEY }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// A duplicate public key is **409 `peer_exists`**, following the WiFi
// collection's `ssid_exists` and not the SSH collection's 422.
#[tokio::test]
pub(super) async fn a_duplicate_peer_is_409_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer_json(
        &router,
        "POST",
        &peers_url("wg0"),
        &token,
        &json!({ "publicKey": PEER_KEY, "allowedIps": ["10.9.0.0/24"] }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "peer_exists");
    assert_eq!(error["path"], json!("network.wg0.wireguard.peers"));
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Section 2.4's rule on the peer item route, with both halves live.
#[tokio::test]
pub(super) async fn an_absent_peer_key_is_404_and_a_malformed_one_is_422() {
    let (router, fake, _cookie, token) = kinds_app().await;

    // Well formed -- it is 32 bytes of base64 -- and no stored peer has it.
    let response = bearer(&router, "DELETE", &peer_url("wg0", OTHER_PEER_KEY), &token).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["path"], json!("network.wg0.wireguard.peers"));

    // Not a public key at all, and could never be one.
    for identifier in ["nope", "AAAA", &"A".repeat(44), &"!".repeat(44)] {
        let response = bearer(&router, "DELETE", &peer_url("wg0", identifier), &token).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{identifier}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{identifier}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The API answers **404** where the pane answers 422, on the same condition.

// A public key carrying a `/` is addressable, percent-encoded.
#[tokio::test]
pub(super) async fn a_peer_key_carrying_a_slash_is_addressable_percent_encoded() {
    let mut tree = kinds_tree("hunter2secret");
    tree["network"]["wg0"]["wireguard"]["peers"] = json!([{ "publicKey": SLASHED_PEER_KEY }]);
    let (tree, token) = with_token(tree);
    let (router, fake) = test_app(tree);
    let cookie = login(&router, "hunter2secret").await;

    assert!(SLASHED_PEER_KEY.contains('/'), "the fixture must carry one");
    let response = bearer(
        &router,
        "DELETE",
        &peer_url("wg0", SLASHED_PEER_KEY),
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.get_settings("network.wg0.wireguard.peers")
            .await
            .unwrap(),
        json!([])
    );

    // Unencoded, the same key is two segments and is not this route. The
    // COOKIE and not the bearer: an undeclared path reaches the gate, and the
    // gate takes the session and only the session.
    let response = request(
        &router,
        "DELETE",
        &format!("{}/{SLASHED_PEER_KEY}", peers_url("wg0")),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(envelope(response).await["code"], "not_found");
}

// A peer the reconciler would refuse is refused here first, and the refusal
// never echoes the key -- the property the reconciler's index-only rule
// exists for, since the message reaches an HTTP client.
#[tokio::test]
pub(super) async fn the_peer_add_runs_the_same_validator_the_reconciler_runs() {
    const PASTED_SECRET: &str = "OOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOO";
    let (router, fake, _cookie, token) = kinds_app().await;

    for (body, fragment) in [
        (
            json!({ "publicKey": PASTED_SECRET }),
            "is not a WireGuard key",
        ),
        (
            json!({ "publicKey": OTHER_PEER_KEY, "allowedIps": ["not-a-cidr"] }),
            "is not an IP address or CIDR",
        ),
        (
            json!({ "publicKey": OTHER_PEER_KEY, "endpoint": "no-port" }),
            "is not host:port",
        ),
    ] {
        let response = bearer_json(
            &router,
            "POST",
            &peers_url("wg0"),
            &token,
            &body.to_string(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        let error = envelope(response).await;
        assert!(
            error["message"].as_str().unwrap().contains(fragment),
            "{body} did not explain itself: {error}"
        );
        assert!(
            !error["message"].as_str().unwrap().contains(PASTED_SECRET),
            "the refusal echoed the value: {error}"
        );
    }

    // A body that is not a peer at all is 422; one that is not JSON is 400.
    for (body, status) in [
        ("{\"nosuchfield\":1}", StatusCode::UNPROCESSABLE_ENTITY),
        ("{", StatusCode::BAD_REQUEST),
    ] {
        let response = bearer_json(&router, "POST", &peers_url("wg0"), &token, body).await;
        assert_eq!(response.status(), status, "{body}");
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// An entry this build cannot read stops every route in the cluster, rather
// than being silently dropped.
#[tokio::test]
pub(super) async fn an_unreadable_network_entry_stops_every_route_in_the_cluster() {
    let mut tree = kinds_tree("hunter2secret");
    tree["network"]["mangled"] = json!("not an interface");
    let (tree, token) = with_token(tree);
    let (router, fake) = test_app(tree);

    for (method, path) in [
        ("PUT", iface_url("eth0")),
        ("DELETE", iface_url("eth0")),
        ("GET", peers_url("wg0")),
    ] {
        let response = if method == "PUT" {
            bearer_json(&router, "PUT", &path, &token, "{\"dhcp\":true}").await
        } else {
            bearer(&router, method, &path, &token).await
        };
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{method} {path}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "settings_invalid", "{method} {path}");
        assert!(
            error["message"].as_str().unwrap().contains("mangled"),
            "the envelope must name the entry: {error}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());

    // The whole-map `PUT` is the exception, and deliberately: it does not read
    // the stored map at all, because the map it sends is the map that ends up
    // stored. It is also the only way out of this state through the API.
    let response = bearer_json(
        &router,
        "PUT",
        NETWORK_MAP_PATH,
        &token,
        &json!({ "eth0": { "dhcp": true } }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

// The four network routes take a bearer **or** a cookie, and answer the
// error envelope at 401 with neither -- never the gate's redirect.

// The gate hands off exactly what the router serves under this prefix, and
// nothing else.
#[tokio::test]
pub(super) async fn the_network_paths_the_router_does_not_serve_reach_the_reservation() {
    let (router, _, cookie, _token) = kinds_app().await;

    for (method, path) in [
        ("GET", "/api/v1/network/"),
        ("DELETE", "/api/v1/network/"),
        ("DELETE", "/api/v1/network/wg0/peers/"),
        ("GET", "/api/v1/network/wg0/peers/extra/deep"),
        ("GET", "/api/v1/network/wg0/notpeers"),
    ] {
        let response = request(&router, method, path, Some(&cookie), None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(
            envelope(response).await["code"],
            "not_found",
            "{method} {path}"
        );
    }

    // The empty interface in the middle IS a route, so an unauthenticated call
    // gets the error envelope and not the gate's redirect -- the same
    // property the rotate action already has.
    let (fresh, _) = test_app(kinds_tree("hunter2secret"));
    let response = request(&fresh, "GET", "/api/v1/network//peers", None, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers().get(LOCATION), None);
    assert_eq!(envelope(response).await["code"], "not_authenticated");
}

// The document describes every network operation, with every
// outcome each has: a client reading only `openapi.json` has to learn them.
