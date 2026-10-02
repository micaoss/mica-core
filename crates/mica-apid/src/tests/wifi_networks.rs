//! The known Wi-Fi networks.

use crate::settings_api::{FakeSettings, SettingsApi};
use axum::http::StatusCode;
use serde_json::json;

use super::*;

// A configured tree carrying a `wifi.client` subtree holding `networks`.
pub(super) fn wifi_tree(networks: serde_json::Value) -> serde_json::Value {
    let mut tree = configured_tree("hunter2secret");
    tree["wifi"] = json!({
        "client": { "enabled": false, "interface": "wlan0", "networks": networks },
        "ap": { "mode": "off", "channel": 6 },
    });
    tree
}

// The stored network list, as JSON.
pub(super) async fn stored_network_list(fake: &FakeSettings) -> serde_json::Value {
    fake.get_settings(WIFI_NETWORKS_DOT_PATH).await.unwrap()
}

// The WiFi collection end to end. **It has no pane**, so this is the first
// management surface the list has ever had: it exists in the settings model
// and was reachable only by editing the settings file on STATE.
#[tokio::test]
pub(super) async fn the_wifi_network_collection_lists_adds_and_removes() {
    let (tree, token) = with_token(wifi_tree(json!([])));
    let (router, fake) = test_app(tree);

    let empty = bearer(&router, "GET", "/api/v1/wifi/client/networks", &token).await;
    assert_eq!(empty.status(), StatusCode::OK);
    assert_eq!(body_json(empty).await, json!([]));

    let added = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        &json!({ "ssid": "roastery", "psk": "hunter2hunter2", "hidden": true, "priority": 7 })
            .to_string(),
    )
    .await;
    assert_eq!(added.status(), StatusCode::CREATED);
    assert_eq!(
        body_json(added).await,
        json!({ "ssid": "roastery", "psk": REDACTED, "hidden": true, "priority": 7 })
    );
    assert_eq!(fake.set_paths(), vec![WIFI_NETWORKS_DOT_PATH]);
    // The stored value is the real key; only what leaves the device is
    // substituted.
    assert_eq!(
        stored_network_list(&fake).await[0]["psk"],
        json!("hunter2hunter2")
    );

    // An open network: `psk` is absent rather than null, which is the model's
    // own shape.
    let open = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        &json!({ "ssid": "cafe-guest" }).to_string(),
    )
    .await;
    assert_eq!(open.status(), StatusCode::CREATED);
    assert_eq!(
        body_json(open).await,
        json!({ "ssid": "cafe-guest", "hidden": false, "priority": 0 })
    );

    let removed = bearer(
        &router,
        "DELETE",
        "/api/v1/wifi/client/networks/roastery",
        &token,
    )
    .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    let left =
        body_json(bearer(&router, "GET", "/api/v1/wifi/client/networks", &token).await).await;
    assert_eq!(
        left,
        json!([{ "ssid": "cafe-guest", "hidden": false, "priority": 0 }])
    );
}

// A key written through `POST` is redacted on the next `GET`, and the
// redaction is the structural one rather than a rule this route
// keeps for itself.
#[tokio::test]
pub(super) async fn a_posted_psk_is_redacted_on_the_next_read() {
    let (tree, token) = with_token(wifi_tree(json!([])));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        &json!({ "ssid": "roastery", "psk": "hunter2hunter2" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let listed = bearer(&router, "GET", "/api/v1/wifi/client/networks", &token).await;
    let body = body_string(listed).await;
    assert!(
        !body.contains("hunter2hunter2"),
        "the stored key left the device: {body}"
    );
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(listed[0]["psk"], json!(REDACTED));

    // The same list read through the settings root is redacted too, which is
    // what makes this one list with one rule and not two surfaces with two.
    let through_settings = bearer(
        &router,
        "GET",
        "/api/v1/settings/wifi.client.networks",
        &token,
    )
    .await;
    assert_eq!(body_json(through_settings).await[0]["psk"], json!(REDACTED));
    assert_eq!(
        stored_network_list(&fake).await[0]["psk"],
        json!("hunter2hunter2")
    );
}

// The round trip that would destroy a working key: read the list, change one
// field, post the entry back. What comes back carries `"<redacted>"` in
// `psk`, and storing it would replace the key with ten literal characters.
#[tokio::test]
pub(super) async fn posting_a_redacted_psk_back_is_refused_and_the_stored_key_survives() {
    let (tree, token) = with_token(wifi_tree(json!([
        { "ssid": "roastery", "psk": "hunter2hunter2", "hidden": false, "priority": 0 },
    ])));
    let (router, fake) = test_app(tree);

    // Exactly what a client that read the collection holds.
    let listed =
        body_json(bearer(&router, "GET", "/api/v1/wifi/client/networks", &token).await).await;
    let mut edited = listed[0].clone();
    edited["ssid"] = json!("roastery-5g");
    edited["hidden"] = json!(true);
    assert_eq!(edited["psk"], json!(REDACTED));

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        &edited.to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "validation_failed");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!(WIFI_NETWORKS_DOT_PATH));

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(
        stored_network_list(&fake).await,
        json!([{ "ssid": "roastery", "psk": "hunter2hunter2", "hidden": false, "priority": 0 }]),
        "the real key must survive the refusal"
    );
}

// The SSID is this collection's identity, so a second entry under one SSID is
// refused rather than appended: with two, a `DELETE` would have no answer to
// which of them it names.
#[tokio::test]
pub(super) async fn a_second_network_under_one_ssid_is_refused() {
    let (tree, token) = with_token(wifi_tree(json!([
        { "ssid": "roastery", "psk": "hunter2hunter2", "hidden": false, "priority": 0 },
    ])));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        &json!({ "ssid": "roastery", "psk": "adifferentkey" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "ssid_exists");
    assert_eq!(error["path"], json!(WIFI_NETWORKS_DOT_PATH));
    assert!(
        !body_string(bearer(&router, "GET", "/api/v1/wifi/client/networks", &token).await)
            .await
            .contains("adifferentkey")
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// A body that is not a network is 422, and the validator is the settings
// model's own deserializer -- which is what micad's `Settings::set` validates
// with, so a body this route accepts is one the store accepts.
#[tokio::test]
pub(super) async fn a_body_that_is_not_a_network_is_422() {
    let (tree, token) = with_token(wifi_tree(json!([])));
    let (router, fake) = test_app(tree);

    for body in [
        // No `ssid`: the one field with no default.
        r#"{"psk":"hunter2hunter2"}"#,
        // A field the model does not carry; `deny_unknown_fields` is what
        // catches a typo before it becomes a silently ignored setting.
        r#"{"ssid":"roastery","hiden":true}"#,
        // Wrong types.
        r#"{"ssid":7}"#,
        r#"{"ssid":"roastery","priority":"high"}"#,
        // An array where an object belongs.
        r#"[{"ssid":"roastery"}]"#,
    ] {
        let response = bearer_json(
            &router,
            "POST",
            "/api/v1/wifi/client/networks",
            &token,
            body,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{body}"
        );
    }

    // Not JSON at all is 400 and not 422: the request could not be read, which
    // is a different failure from one that was read and refused.
    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/wifi/client/networks",
        &token,
        "{not json",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(envelope(response).await["code"], "request_invalid");

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// Section 2.4's rule on the WiFi item route, with its 422 half **vacant**.
//
// An SSID has no grammar -- every non-empty single path segment spells a
// possible one -- so there is no malformed identifier to answer 422 about and
// everything absent is 404. That is the rule applied, not an exception to it.
#[tokio::test]
pub(super) async fn an_absent_ssid_is_404_and_this_collection_has_no_pane_to_disagree_with() {
    let (tree, token) = with_token(wifi_tree(json!([
        { "ssid": "roastery", "psk": "hunter2hunter2", "hidden": false, "priority": 0 },
    ])));
    let (router, fake) = test_app(tree);

    for ssid in ["cafe-guest", "roastery-5g", "%20", "SHA256:not-an-ssid"] {
        let response = bearer(
            &router,
            "DELETE",
            &format!("/api/v1/wifi/client/networks/{ssid}"),
            &token,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{ssid}");
        let error = envelope(response).await;
        assert_eq!(error["code"], "settings_not_found", "{ssid}");
        assert_eq!(error["source"], "apid", "{ssid}");
        assert_eq!(error["path"], json!(WIFI_NETWORKS_DOT_PATH), "{ssid}");
    }

    // No pane serves this list -- the assertion behind the paragraph above, so
    // it cannot quietly stop being true. Read out of the router's own source,
    // for the reason `every_mutating_route_is_covered_by_the_authentication_tests`
    // reads it: a hand-listed set of paths to probe would describe the panes
    // somebody remembered.
    let html_wifi_routes: Vec<&str> = include_str!("../routes/router.rs")
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(".route(\"/wifi"))
        .collect();
    assert!(
        html_wifi_routes.is_empty(),
        "this collection now has a pane, so the paragraph above is stale and a paired 422/404 test is owed: {html_wifi_routes:?}"
    );

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// Both collections take a bearer token **and** a session cookie, and neither
// takes nothing.

// The document describes the served surface: a client reading only
// `openapi.json` has to learn both collections, every outcome each route has,
// and that the SSH listing carries a `notice`.
#[test]
pub(super) fn the_openapi_document_covers_the_two_collections() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    for (path, method, statuses) in [
        (
            "/api/v1/ssh/authorized-keys",
            "get",
            vec!["200", "401", "405", "500", "503"],
        ),
        (
            "/api/v1/ssh/authorized-keys",
            "post",
            vec!["201", "400", "401", "405", "409", "422", "500", "503"],
        ),
        (
            "/api/v1/ssh/authorized-keys/{fingerprint}",
            "delete",
            vec!["204", "401", "404", "405", "422", "500", "503"],
        ),
        (
            "/api/v1/wifi/client/networks",
            "get",
            vec!["200", "401", "405", "500", "503"],
        ),
        (
            "/api/v1/wifi/client/networks",
            "post",
            vec!["201", "400", "401", "405", "409", "422", "500", "503"],
        ),
        (
            "/api/v1/wifi/client/networks/{ssid}",
            "delete",
            vec!["204", "401", "404", "405", "500", "503"],
        ),
    ] {
        let operation = &document["paths"][path][method];
        assert!(operation.is_object(), "{method} {path} is undocumented");
        for status in statuses {
            assert!(
                operation["responses"][status].is_object(),
                "{method} {path} must document {status}: {operation}"
            );
        }
    }

    // The notice is a documented member and not an undeclared extra, on both
    // answers that carry it.
    let schemas = &document["components"]["schemas"];
    assert!(schemas["AuthorizedKeyList"]["properties"]["notice"].is_object());
    assert!(schemas["AddedAuthorizedKey"]["properties"]["notice"].is_object());
    // The WiFi item route documents no 422: its identifier has no grammar, so
    // there is no malformed spelling to answer one for.
    assert!(
        document["paths"]["/api/v1/wifi/client/networks/{ssid}"]["delete"]["responses"]["422"]
            .is_null()
    );
}

// The documented WiFi entry is the settings model's own shape.
#[test]
pub(super) fn the_wifi_schema_matches_the_settings_model() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");
    let mut documented: Vec<String> =
        document["components"]["schemas"]["WifiNetworkEntry"]["properties"]
            .as_object()
            .expect("WifiNetworkEntry is an object schema")
            .keys()
            .cloned()
            .collect();

    // Every field present: `psk` is the one the model omits when it is absent.
    let model = serde_json::to_value(micad_settings::WifiNetwork {
        ssid: "roastery".to_string(),
        psk: Some("hunter2hunter2".to_string()),
        hidden: true,
        priority: 7,
    })
    .expect("a network serializes");
    let mut fields: Vec<String> = model
        .as_object()
        .expect("a network is an object")
        .keys()
        .cloned()
        .collect();
    fields.sort();
    documented.sort();
    assert_eq!(
        documented, fields,
        "the documented WiFi entry has drifted from `micad_settings::WifiNetwork`"
    );
}

// The network cluster typed, the
// WireGuard peer collection, and the rotate-key 404.

// The lifted pre-shared key bound, run by the WiFi route for the first time.
#[tokio::test]
pub(super) async fn a_psk_outside_the_lifted_bounds_is_refused_by_the_wifi_route() {
    let (tree, token) = with_token(wifi_tree(json!([])));
    let (router, fake) = test_app(tree);

    for psk in ["short07", &"x".repeat(64)] {
        let response = bearer_json(
            &router,
            "POST",
            "/api/v1/wifi/client/networks",
            &token,
            &json!({ "ssid": "roastery", "psk": psk }).to_string(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{} characters",
            psk.len()
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed");
        assert!(
            error["message"].as_str().unwrap().contains("8 to 63"),
            "{error}"
        );
        // The message never names the length observed: a length is a fact
        // about a secret, and this string reaches an HTTP client.
        assert!(
            !error["message"].as_str().unwrap().contains(psk),
            "the refusal echoed the key: {error}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());

    // And the two admissible shapes still store: a passphrase in range, and a
    // 64-digit hex PMK, which the bound does not apply to.
    for (ssid, psk) in [("roastery", "hunter2hunter2"), ("lab", &"a".repeat(64))] {
        let response = bearer_json(
            &router,
            "POST",
            "/api/v1/wifi/client/networks",
            &token,
            &json!({ "ssid": ssid, "psk": psk }).to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{ssid}");
    }
}

// The quotable half of the lifted bound, run by the WiFi route.
#[tokio::test]
pub(super) async fn a_psk_the_renderer_cannot_quote_is_refused_by_the_wifi_route() {
    let (tree, token) = with_token(wifi_tree(json!([])));
    let (router, fake) = test_app(tree);

    for psk in [
        "has\"quote1",
        "has\\backslash",
        "two\nlines1",
        "tab\there1",
        "caf\u{e9}-latte",
    ] {
        let response = bearer_json(
            &router,
            "POST",
            "/api/v1/wifi/client/networks",
            &token,
            &json!({ "ssid": "roastery", "psk": psk }).to_string(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{psk:?}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed", "{psk:?}");
        assert_eq!(error["source"], "apid", "{psk:?}");
        assert_eq!(error["path"], json!(WIFI_NETWORKS_DOT_PATH), "{psk:?}");
        // The refusal never echoes the key.
        assert!(
            !error["message"].as_str().unwrap().contains(psk),
            "the refusal echoed the key: {error}"
        );
    }
    // Nothing reached micad: a key refused here is never stored, which is the
    // whole point of moving the refusal to the write surface.
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}
