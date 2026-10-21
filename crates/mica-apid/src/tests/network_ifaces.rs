//! Network interfaces: VLAN, bridge, addressing and the whole map.

use crate::settings_api::{FakeSettings, SettingsApi};
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use serde_json::json;

use super::*;

// The API path of one interface.
pub(super) const NETWORK_MAP_PATH: &str = "/api/v1/network";

// The `network` dot-path every envelope about the whole map names.
pub(super) const NETWORK_DOT_PATH: &str = "network";

pub(super) fn iface_url(iface: &str) -> String {
    format!("{NETWORK_MAP_PATH}/{}", urlencode(iface))
}

pub(super) fn peers_url(iface: &str) -> String {
    format!("{}/peers", iface_url(iface))
}

pub(super) fn peer_url(iface: &str, public_key: &str) -> String {
    format!("{}/{}", peers_url(iface), urlencode(public_key))
}

// The stored map, read back through the fake.
pub(super) async fn stored_network_map(fake: &FakeSettings) -> serde_json::Value {
    fake.get_settings(NETWORK_DOT_PATH).await.unwrap()
}

// The whole map, written at the version a read just answered.
pub(super) async fn put_network_map(
    router: &Router,
    token: &str,
    map: &serde_json::Value,
) -> axum::response::Response {
    let read = body_json(bearer(router, "GET", NETWORK_MAP_PATH, token).await).await;
    let body = json!({ "baseVersion": read["version"], "configured": map });
    bearer_json(router, "PUT", NETWORK_MAP_PATH, token, &body.to_string()).await
}

// A syntactically valid X25519 public key whose base64 spelling carries a
// `/`, which the standard alphabet really does contain.
//
// Its private half was never generated -- it is 32 copies of one byte -- so
// it authorises nothing anywhere.
pub(super) const SLASHED_PEER_KEY: &str = "Pz8/Pz8/Pz8/Pz8/Pz8/Pz8/Pz8/Pz8/Pz8/Pz8/Pz8=";

// The four relational rules, each with its own route-level test, because the
// whole reason this cluster is typed rather than a dot-path passthrough is
// that these rules exist and a passthrough runs none of them.
#[tokio::test]
pub(super) async fn a_vlan_parent_that_is_not_declared_is_422_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("vlan9"),
        &token,
        &json!({ "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth9", "id": 9 } }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_api_headers(&response, "vlan parent");
    let error = envelope(response).await;
    assert_eq!(error["code"], "vlan_parent_invalid");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!("network.vlan9"));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("has VLAN parent \"eth9\", which is not a declared network entry"),
        "{error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Rule two. This is the exact submission the contract
// names as the concrete failure a bare passthrough produces: a bridge naming
// a port that does not exist, which a `PUT` to
// `/api/v1/settings/network.br9` would have answered 204 to.
#[tokio::test]
pub(super) async fn a_bridge_port_that_is_not_declared_is_422_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("br9"),
        &token,
        &json!({ "kind": "bridge", "dhcp": true, "bridge": { "ports": ["eth9"] } }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "bridge_port_invalid");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("has bridge port \"eth9\", which is not a declared network entry"),
        "{error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Rule three, and it is the one no check confined to the entry being written
// could ever see: what is refused here is an edit to `eth1`, and what refuses
// it is `br0`, a different entry that claims `eth1` as a port.
#[tokio::test]
pub(super) async fn a_bridge_port_that_carries_addressing_is_422_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth1"),
        &token,
        &json!({ "dhcp": true }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert!(
        error["message"].as_str().unwrap().contains(
            "network.eth1 is a port of bridge br0 and must not carry addressing of its own"
        ),
        "{error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Rule four. `br0` already claims `eth1`; a second bridge claiming it is a
// race between two `Bridge=` lines for one file, and it is refused.
#[tokio::test]
pub(super) async fn a_port_claimed_by_two_bridges_is_422_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("br1"),
        &token,
        &json!({ "kind": "bridge", "dhcp": true, "bridge": { "ports": ["eth1"] } }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("is claimed as a port by both bridge"),
        "{error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// The happy path: declare an interface that did not exist, replace one that
// did, and remove one.
#[tokio::test]
pub(super) async fn the_interface_route_declares_replaces_and_removes() {
    let (router, fake, _cookie, token) = kinds_app().await;

    // Declared: `eth2` is not in the stored map, and a `PUT` creates it.
    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth2"),
        &token,
        &json!({ "dhcp": false, "static": { "address": "10.0.0.9/24", "dns": ["1.1.1.1"] } })
            .to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(header_value(&response, CACHE_CONTROL), "no-store");
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), vec!["network.eth2".to_string()]);
    assert_eq!(
        fake.get_settings("network.eth2").await.unwrap(),
        json!({ "dhcp": false, "static": { "address": "10.0.0.9/24", "dns": ["1.1.1.1"] } })
    );

    // Replaced whole: the second body has no `static`, and the stored entry
    // has none afterwards. A `PUT` is the entry, not a patch of it.
    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth2"),
        &token,
        &json!({ "dhcp": true }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.get_settings("network.eth2").await.unwrap(),
        json!({ "dhcp": true })
    );

    // Removed: the whole map is rewritten without it, because the dot-path
    // syntax has no delete.
    let response = bearer(&router, "DELETE", &iface_url("eth2"), &token).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.set_paths().last().map(String::as_str),
        Some(NETWORK_DOT_PATH)
    );
    let map = stored_network_map(&fake).await;
    assert!(map.get("eth2").is_none(), "{map}");
    // And nothing else went with it.
    for kept in ["eth0", "eth1", "eth0.100", "br0", "wg0"] {
        assert!(map.get(kept).is_some(), "{kept} was dropped: {map}");
    }
}

// A removal is re-validated against the map it leaves behind, which is the
// half a delete-by-dot-path could not do at all.
#[tokio::test]
pub(super) async fn removing_a_port_a_bridge_still_lists_is_refused() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let response = bearer(&router, "DELETE", &iface_url("eth1"), &token).await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "in_use");
    assert_eq!(error["params"], json!({ "name": "eth1", "refs": "br0" }));
    assert_eq!(
        error["details"],
        json!([{ "interface": "eth1", "field": "name", "code": "in_use", "params": { "name": "eth1", "refs": "br0" } }])
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);

    // Removing the bridge first makes the port removable, which is the order
    // the message asks for.
    assert_eq!(
        bearer(&router, "DELETE", &iface_url("br0"), &token)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        bearer(&router, "DELETE", &iface_url("eth1"), &token)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
}

// Section 2.4's rule on the interface item route: absent is 404, malformed is
// 422, and they are not the same answer.
//
// There is no 404 on the `PUT`, deliberately: that route's job is to create
// the entry it names, so an absent one is not an absent resource.
#[tokio::test]
pub(super) async fn an_absent_interface_is_404_and_a_malformed_name_is_422() {
    let (router, fake, _cookie, token) = kinds_app().await;

    let response = bearer(&router, "DELETE", &iface_url("eth9"), &token).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_api_headers(&response, "absent interface");
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!(NETWORK_DOT_PATH));

    // Not a name any interface could have: sixteen characters is one past
    // `IFNAMSIZ` minus the terminator, and `/` is not in the charset (it
    // reaches the route percent-encoded, so it is one segment).
    for bad in ["waytoolongiface016", "bad%2Fname"] {
        for method in ["PUT", "DELETE"] {
            let path = format!("{NETWORK_MAP_PATH}/{bad}");
            let response = if method == "PUT" {
                bearer_json(&router, "PUT", &path, &token, "{\"dhcp\":true}").await
            } else {
                bearer(&router, method, &path, &token).await
            };
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{method} {bad}"
            );
            assert_eq!(
                envelope(response).await["code"],
                "validation_failed",
                "{method} {bad}"
            );
        }
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The whole map, replaced in one request and validated as one tree.
#[tokio::test]
pub(super) async fn the_whole_map_put_replaces_atomically_and_validates_relationally() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    // Refused as one tree: `br9` names a port that this very body does not
    // declare either.
    let response = put_network_map(
        &router,
        &token,
        &json!({
            "eth0": { "dhcp": true },
            "br9": { "kind": "bridge", "dhcp": true, "bridge": { "ports": ["eth7"] } },
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "bridge_port_invalid");
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);

    // Accepted as one tree: the same bridge, with its port declared in the
    // same body. Neither entry is legal without the other.
    let response = put_network_map(
        &router,
        &token,
        &json!({
            "eth7": { "dhcp": false },
            "br9": { "kind": "bridge", "dhcp": true, "bridge": { "ports": ["eth7"] } },
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec![NETWORK_DOT_PATH.to_string()]);
    // Replaced and not merged: every entry the old map had is gone.
    let map = stored_network_map(&fake).await;
    assert_eq!(
        map.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["br9", "eth7"],
        "{map}"
    );

    // A key that is not an interface name is 422, and it names the key.
    let response = put_network_map(
        &router,
        &token,
        &json!({ "waytoolongiface016": { "dhcp": true } }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "name_invalid");
    assert_eq!(error["details"][0]["interface"], "waytoolongiface016");

    // A body that is not a map of interfaces at all is 422; a body that is not
    // JSON is 400.
    for (body, status) in [
        ("[]", StatusCode::UNPROCESSABLE_ENTITY),
        (
            "{\"baseVersion\":\"x\",\"configured\":{\"eth0\":{\"nosuchfield\":1}}}",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // The bare map, without the version it was read at.
        (
            "{\"eth0\":{\"dhcp\":true}}",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("{", StatusCode::BAD_REQUEST),
    ] {
        let response = bearer_json(&router, "PUT", NETWORK_MAP_PATH, &token, body).await;
        assert_eq!(response.status(), status, "{body}");
    }
}

// Both typed write paths run the
// wizard's CIDR rule, on the entries the *request* carries.
#[tokio::test]
pub(super) async fn the_typed_network_writes_refuse_an_address_that_is_not_a_cidr() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    // The item route: an address the kernel cannot parse, on one entry.
    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth0"),
        &token,
        &json!({ "dhcp": false, "static": { "address": "192.168.1.10" } }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_api_headers(
        &response,
        "an item write carrying an address that is not a CIDR",
    );
    let error = envelope(response).await;
    assert_eq!(error["code"], "validation_failed");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!("network.eth0"));
    assert_eq!(
        error["message"],
        json!("Static address must be IPv4 CIDR notation, e.g. 192.168.1.10/24."),
        "the message is the wizard's own, so the two surfaces do not disagree"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);

    // The map route: one bad entry refuses the whole body, and the detail
    // names that entry, because that is what failed.
    let response = put_network_map(
        &router,
        &token,
        &json!({
            "eth0": { "dhcp": true },
            "eth1": { "dhcp": false, "static": { "address": "10.0.0.5/33" } },
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "validation_failed");
    assert_eq!(error["path"], json!("network"));
    assert_eq!(error["details"][0]["interface"], "eth1");
    assert_eq!(error["details"][0]["field"], "static.address");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("IPv4 CIDR notation"),
        "{error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);

    // The two arms the condition does not reach: DHCP on with a junk address
    // left in the block, and DHCP off with no `static` at all -- which is a
    // bridge port, an interface with no addressing rather than an error.
    for (iface, body) in [
        (
            "eth0",
            json!({ "dhcp": true, "static": { "address": "nonsense" } }),
        ),
        ("eth1", json!({ "dhcp": false })),
    ] {
        let response =
            bearer_json(&router, "PUT", &iface_url(iface), &token, &body.to_string()).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{iface} {body}");
    }
}

// A dotted interface name round-trips through the quoted path segment, so the
// daemon sees one key and not two.
#[tokio::test]
pub(super) async fn a_dotted_interface_name_round_trips_through_the_quoted_path_segment() {
    let (router, fake, _cookie, token) = kinds_app().await;

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth0.100"),
        &token,
        &json!({ "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth0", "id": 100 } })
            .to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec![r#"network."eth0.100""#.to_string()]);

    // And the error envelope quotes it too, because that is the dot-path an operator
    // would type at the settings route.
    let (router, _, _cookie, token) = kinds_app().await;
    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("wg.9"),
        &token,
        &json!({ "kind": "vlan", "dhcp": true, "vlan": { "parent": "nope", "id": 1 } }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["path"], json!(r#"network."wg.9""#));
}

// A raw settings write under `network` is 409 and names the typed routes.
#[tokio::test]
pub(super) async fn the_settings_passthrough_under_network_is_409_and_names_the_typed_route() {
    let (router, fake, _cookie, token) = kinds_app().await;

    for path in [
        "/api/v1/settings/network",
        "/api/v1/settings/network.br0",
        "/api/v1/settings/network.br0.bridge.ports",
    ] {
        let response = bearer_json(&router, "PUT", path, &token, "{\"dhcp\":true}").await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
        let error = envelope(response).await;
        assert_eq!(error["code"], "settings_read_only", "{path}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("/api/v1/network"),
            "{path} did not name the typed route: {error}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// A CAN interface is written like any other entry, its block alone.
#[tokio::test]
pub(super) async fn a_can_interface_is_written_with_its_can_block() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let body =
        json!({ "kind": "can", "can": { "bitrate": 250000, "restartMs": 100, "fd": false } });

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("can0"),
        &token,
        &body.to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec!["network.can0".to_string()]);
    assert_eq!(
        stored_network_map(&fake).await["can0"],
        json!({ "kind": "can", "dhcp": false, "can": { "bitrate": 250000, "restartMs": 100, "fd": false } })
    );
}

// A CAN entry out of its rules is 422 with the rule's own sentence, and
// nothing is written.
#[tokio::test]
pub(super) async fn a_can_interface_out_of_its_rules_is_422_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    for (body, needle) in [
        (
            json!({ "kind": "can", "can": { "bitrate": 2000000 } }),
            "can.bitrate 2000000",
        ),
        (
            json!({ "kind": "can", "can": { "bitrate": 500000, "fd": true } }),
            "needs can.dataBitrate",
        ),
        (
            json!({ "kind": "can", "dhcp": true, "can": { "bitrate": 500000 } }),
            "carries its can block alone",
        ),
        (json!({ "kind": "can" }), "carries no can block"),
    ] {
        let response = bearer_json(
            &router,
            "PUT",
            &iface_url("can0"),
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
        assert_eq!(error["code"], "validation_failed", "{body}");
        assert!(
            error["message"].as_str().unwrap().contains(needle),
            "{body}: {error}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// A write names the version it was read at: once the map moves, the same
// version is 409 and nothing is written.
#[tokio::test]
pub(super) async fn the_whole_map_put_names_the_version_it_was_read_at() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let response = bearer(&router, "GET", NETWORK_MAP_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let version = body_json(response).await["version"].clone();
    assert!(version.as_str().is_some_and(|version| !version.is_empty()));

    let put = |body: serde_json::Value| {
        let (router, token) = (router.clone(), token.clone());
        async move { bearer_json(&router, "PUT", NETWORK_MAP_PATH, &token, &body.to_string()).await }
    };
    let response =
        put(json!({ "baseVersion": version, "configured": { "eth0": { "dhcp": true } } })).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted = body_json(response).await;
    assert!(accepted["taskId"].is_string(), "{accepted}");
    assert_ne!(accepted["version"], version, "{accepted}");
    let read = body_json(bearer(&router, "GET", NETWORK_MAP_PATH, &token).await).await;
    assert_eq!(read["version"], accepted["version"]);
    let after = stored_network_map(&fake).await;

    let response =
        put(json!({ "baseVersion": version, "configured": { "eth1": { "dhcp": true } } })).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "version_conflict");
    assert_eq!(error["path"], json!(NETWORK_DOT_PATH));
    assert_eq!(stored_network_map(&fake).await, after);
}

// A dry run validates and answers what would change, writing nothing.
#[tokio::test]
pub(super) async fn a_dry_run_answers_the_difference_and_writes_nothing() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let read = body_json(bearer(&router, "GET", NETWORK_MAP_PATH, &token).await).await;
    let mut map = read["configured"].clone();
    map.as_object_mut().unwrap().remove("wg0");
    map["eth9"] = json!({ "dhcp": true });
    map["eth0.100"]["static"]["address"] = json!("192.168.100.3/24");
    let body = json!({ "baseVersion": read["version"], "configured": map });

    let url = format!("{NETWORK_MAP_PATH}?dryRun=true");
    let response = bearer_json(&router, "PUT", &url, &token, &body.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let answer = body_json(response).await;
    assert_eq!(answer["adopted"], json!([]));
    assert_eq!(answer["warnings"], json!([]));
    let diff = answer["diff"].as_array().unwrap();
    assert!(
        diff.contains(&json!({ "interface": "eth9", "change": "added" })),
        "{answer}"
    );
    assert!(
        diff.contains(&json!({ "interface": "wg0", "change": "removed" })),
        "{answer}"
    );
    assert!(
        diff.contains(&json!({ "interface": "eth0.100", "change": "modified" })),
        "{answer}"
    );
    assert_eq!(diff.len(), 3, "{answer}");
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());

    // A dry run refuses what the write would.
    let body = json!({ "baseVersion": read["version"], "configured": { "eth0": { "dhcp": false, "static": { "address": "10.0.0.5/33" } } } });
    let response = bearer_json(&router, "PUT", &url, &token, &body.to_string()).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// While a network task has not finished, another network write is 409 busy.
#[tokio::test]
pub(super) async fn a_network_write_waits_for_the_running_network_task() {
    let (tree, token) = with_token(kinds_tree("hunter2secret"));
    let fake = std::sync::Arc::new(FakeSettings::new(tree));
    let state = crate::routes::AppState::new(fake.clone(), SIGNING_KEY);
    let registry = state.task_registry().clone();
    registry.subscribed();
    let task = |id: &str, dot_path: &str, status: &str| crate::task_registry::TaskRecord {
        id: id.to_string(),
        operation: "settings-write".to_string(),
        dot_path: dot_path.to_string(),
        source: ":1.9".to_string(),
        status: status.to_string(),
        enqueued_at: "2026-10-21T00:00:00.000Z".to_string(),
        started_at: None,
        finished_at: None,
        outcome: None,
        message: None,
        folded_count: 0,
    };
    // Another subtree's task holds nothing back.
    registry.update(task("t1", "access.ssh.enabled", "running"));
    let router = crate::routes::app(state);
    let response = put_network_map(&router, &token, &json!({ "eth0": { "dhcp": true } })).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    registry.update(task("t2", "network", "queued"));
    let response = put_network_map(&router, &token, &json!({ "eth1": { "dhcp": true } })).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(envelope(response).await["code"], "busy");
    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth1"),
        &token,
        &json!({ "dhcp": true }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(fake.set_paths().len(), 1, "{:?}", fake.set_paths());

    registry.update(task("t2", "network", "finished"));
    let response = put_network_map(&router, &token, &json!({ "eth1": { "dhcp": true } })).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}
