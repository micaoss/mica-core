//! WireGuard key rotation.

use crate::auth;
use crate::settings_api::{FakeSettings, SettingsApi};
use axum::Router;
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use serde_json::json;
use std::sync::Arc;

use super::*;

// A settings tree with `network` entries of every kind, in the shape schema
// v7 stores them.
pub(super) fn kinds_tree(password: &str) -> serde_json::Value {
    json!({
        "hostname": "mica",
        "network": {
            "eth0": { "dhcp": true },
            "eth1": { "dhcp": false },
            "eth0.100": {
                "kind": "vlan",
                "dhcp": false,
                "static": { "address": "192.168.100.2/24", "dns": [] },
                "vlan": { "parent": "eth0", "id": 100 },
            },
            "br0": { "kind": "bridge", "dhcp": true, "bridge": { "ports": ["eth1"] } },
            "wg0": {
                "kind": "wireguard",
                "dhcp": false,
                "static": { "address": "10.8.0.2/24", "dns": [] },
                "wireguard": {
                    "listenPort": 51820,
                    "peers": [{
                        "publicKey": PEER_KEY,
                        "allowedIps": ["10.8.0.0/24"],
                        "endpoint": "vpn.example.net:51820",
                    }],
                },
            },
        },
        "access": { "webAdmin": { "password_hash": auth::hash_password(password).unwrap() } },
    })
}

// A syntactically valid X25519 public key: 32 bytes in padded base64.
//
// Its private half was never generated — this is 32 constant bytes — so it
// authorises nothing anywhere. It exists so that a rejection in these tests is
// a verdict on the rule under test rather than on the shape of the value.
pub(super) const PEER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

// A second one, distinct from [`PEER_KEY`], for the add/remove tests.
pub(super) const OTHER_PEER_KEY: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

// The live-state object micad's network reconciler publishes for `kinds_tree`,
// including `kind` on every entry and `publicKey` on
// the tunnel.
pub(super) fn network_state() -> serde_json::Value {
    json!({
        "eth0": { "file": "50-mica-eth0.network", "dhcp": true, "kind": "physical" },
        "eth1": { "file": "50-mica-eth1.network", "dhcp": false, "kind": "physical" },
        "eth0.100": { "file": "50-mica-eth0.100.network", "dhcp": false, "kind": "vlan" },
        "br0": { "file": "50-mica-br0.network", "dhcp": true, "kind": "bridge" },
        "wg0": {
            "file": "50-mica-wg0.network",
            "dhcp": false,
            "kind": "wireguard",
            "publicKey": PEER_KEY,
        },
    })
}

// A router over [`kinds_tree`] with [`network_state`] published, plus a
// session cookie for it.
pub(super) async fn kinds_app() -> (Router, Arc<FakeSettings>, String, String) {
    // Both credentials: the panes in this
    // cluster take the cookie and the `/api/v1/network` routes beside them take
    // the bearer, and several tests assert the two surfaces agree. The token is
    // seeded into the tree rather than minted through the pane because most of
    // those tests assert `set_paths` exactly, and a mint is a write.
    let (tree, token) = with_token(kinds_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("network", network_state());
    let cookie = login(&router, "hunter2secret").await;
    (router, fake, cookie, token)
}

// The pane renders one typed form per kind, filled in from the stored entry.

// The live-state reader: `kind` for every entry and `publicKey` for the
// tunnel.

// micad having published no state yet is a fact the pane states, not a 502:
// the stored configuration is still worth showing.

// One entry whose body this pane cannot read must not blank out the others.

// The three virtual kinds, written through the quoted-path writer as the
// typed bodies micad deserializes.

// Saving a tunnel from the form keeps the peers the form does not carry.
//
// The failure this pins is silent: changing a listen port would otherwise
// disconnect every far end, and the pane would report "Settings saved."

// A value left in another kind's box is never written: the form renders all
// four groups at once, and only the group the submitted kind names is read.

// An interface with DHCP off and no address is an interface with no
// addressing, which is exactly what a bridge port is.

// The reconciler's cross-field rules, echoed by the pane for a readable error.
//
// Each row is a rule `validate_network` enforces in micad. The pane is not the
// boundary — the settings file is writable without apid — so this asserts the
// echo, and that nothing was written when it fired.

// Peers are added and removed at the peer list's own dot-path, quoted when
// the interface name carries a dot.

// A dotted tunnel name reaches the peer list as one quoted segment.

// the sweep, settled by running it: the
// pane's peer-add for an interface that is **not a declared network entry**
// neither refuses nor 404s -- it succeeds, and writes a `network.wg9` entry
// of the default kind carrying a WireGuard block.

// A peer the reconciler would refuse is refused here first, and the refusal
// never echoes the key.

// Removing a peer nobody has is an error rather than a silent no-op rewrite.

// Adding a peer twice is refused: two `[WireGuardPeer]` sections with one
// public key is a tunnel whose far end is described twice.

// The rotate-key route.

// The route in the three spellings that have to agree: the constant the
// router registers, what a caller sends, and what the document describes.
pub(super) const ROTATE_PATH: &str = "/api/v1/actions/wireguard/wg0/rotate-key";

// The action route: micad draws the key, and the body carries its public
// half and nothing else.
#[tokio::test]
pub(super) async fn the_rotate_route_answers_the_new_public_key() {
    let (router, fake, _cookie, token) = kinds_app().await;

    let response = bearer_form(&router, ROTATE_PATH, &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, ROTATE_PATH);
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();

    assert_eq!(
        fake.rotations(),
        vec![(
            "wg0".to_string(),
            body["publicKey"].as_str().unwrap().to_string()
        )]
    );
    // The whole body, by identity: a member added here would be a member
    // shipped to every client, and the one member that must never appear is a
    // private key.
    assert_eq!(
        body.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["publicKey"],
        "{body}"
    );
}

// It rotates and it does not write: the settings tree holds no key, so there
// is nothing there for a rotation to change.
#[tokio::test]
pub(super) async fn a_rotation_writes_nothing_to_the_settings_tree() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = fake.get_settings("network.wg0").await.unwrap();

    let response = bearer_form(&router, ROTATE_PATH, &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(fake.get_settings("network.wg0").await.unwrap(), before);
}

// The classification, inherited whole by the action route: micad's fdo error
// name decides the status and the code, and the error envelope names the settings
// dot-path at fault.
#[tokio::test]
pub(super) async fn the_rotate_routes_failures_take_the_shared_envelope() {
    for (fdo_name, code, status) in [
        // the correction, on the apid side: **no apid logic
        // changed**. micad split its one `InvalidArgs` into a not-found for an
        // undeclared entry and an `InvalidArgs` for one of the wrong kind, and
        // the classifier below already mapped both names. This row is the
        // proof that it did.
        (
            Some("com.mica.micad1.Error.NotFound"),
            "settings_not_found",
            StatusCode::NOT_FOUND,
        ),
        (
            Some("org.freedesktop.DBus.Error.InvalidArgs"),
            "settings_rejected",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Some("org.freedesktop.DBus.Error.IOError"),
            "settings_io",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            Some("org.freedesktop.DBus.Error.Failed"),
            "micad_failed",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (None, "micad_unreachable", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let (router, token) = failing_app(fdo_name).await;
        let response = bearer_form(&router, ROTATE_PATH, &token, "").await;
        assert_eq!(response.status(), status, "{fdo_name:?}");
        assert_api_headers(&response, ROTATE_PATH);
        let error = envelope(response).await;
        assert_eq!(error["code"], code, "{fdo_name:?}");
        // The dot-path at fault is the entry whose kind micad refused, not the
        // HTTP path: the member is a settings dot-path.
        assert_eq!(error["path"], json!("network.wg0"), "{fdo_name:?}");
    }
}

// A dotted tunnel name reaches the error envelope as a quoted segment, because that
// is the dot-path an operator would type at the settings route.
#[tokio::test]
pub(super) async fn the_rotate_envelope_quotes_a_dotted_interface_name() {
    let (router, token) = failing_app(Some("org.freedesktop.DBus.Error.InvalidArgs")).await;
    let response = bearer_form(
        &router,
        "/api/v1/actions/wireguard/wg.0/rotate-key",
        &token,
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["path"], json!(r#"network."wg.0""#));
}

// The trap, for the rotate-key route: an unauthenticated call
// answers the error envelope with a 401 and **never** a redirect, in both gate
// modes. It is a POST, so a client that followed the gate's 303 would land on
// `GET /login`, read 200, and believe it had rotated a key.
#[tokio::test]
pub(super) async fn the_rotate_route_is_401_without_a_session_in_both_gate_modes() {
    let (configured, _) = test_app(kinds_tree("hunter2secret"));
    let (fresh, _) = test_app(unconfigured_tree());

    for (mode, router) in [("configured", &configured), ("setup mode", &fresh)] {
        let response = post_form(router, ROTATE_PATH, "", None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{mode}");
        assert_eq!(
            response.headers().get(LOCATION),
            None,
            "{mode} answered a redirect, which a script reads as success"
        );
        assert_api_headers(&response, mode);
        assert_eq!(
            envelope(response).await["code"],
            "not_authenticated",
            "{mode}"
        );
    }
}

// The gate hands off exactly what the router serves, and nothing else: an
// interface name carrying a path separator is not this route.
#[tokio::test]
pub(super) async fn a_rotate_path_with_an_extra_segment_is_the_subtrees_404() {
    let (router, _, cookie, _token) = kinds_app().await;
    for path in [
        "/api/v1/actions/wireguard/a/b/rotate-key",
        "/api/v1/actions/wireguard/wg0/rotate-key/extra",
        "/api/v1/actions/wireguard/wg0",
    ] {
        let response = post_form(&router, path, "", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(envelope(response).await["code"], "not_found", "{path}");
    }
}

// The gate and the router agree about the empty interface segment, which is a
// path this router really serves: `{iface}` matches zero characters where
// `{*path}` matches at least one.
#[tokio::test]
pub(super) async fn the_empty_interface_segment_is_the_route_and_not_a_redirect() {
    const EMPTY: &str = "/api/v1/actions/wireguard//rotate-key";

    let (router, _) = test_app(kinds_tree("hunter2secret"));
    let response = post_form(&router, EMPTY, "", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers().get(LOCATION), None);
    assert_eq!(envelope(response).await["code"], "not_authenticated");

    let (router, token) = failing_app(Some("org.freedesktop.DBus.Error.InvalidArgs")).await;
    let response = bearer_form(&router, EMPTY, &token, "").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["path"], json!("network."));
}

// There is no GET on it. A rotation replaces a tunnel's identity, so nothing
// that merely follows a link may perform one.
#[tokio::test]
pub(super) async fn the_rotate_route_has_no_get() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let response = bearer(&router, "GET", ROTATE_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(fake.rotations().is_empty());
}

// The document describes the rotate-key route, with every outcome it
// has: a client reading only `openapi.json` has to learn them.
#[test]
pub(super) fn the_openapi_document_covers_the_rotate_route() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let responses =
        &document["paths"]["/api/v1/actions/wireguard/{iface}/rotate-key"]["post"]["responses"];
    // The 404 arrived later: an interface that is not a declared entry
    // names nothing, which is what every other read on this API already
    // answered 404 for.
    for status in ["200", "401", "404", "422", "500", "503"] {
        assert!(
            responses[status].is_object(),
            "the rotate route is missing its {status}: {document}"
        );
    }
    // And it is a POST only: a documented GET would be a contract for a route
    // that does not exist.
    assert!(
        document["paths"]["/api/v1/actions/wireguard/{iface}/rotate-key"]["get"].is_null(),
        "{document}"
    );
    // The success body carries the public half and no other member.
    let properties = &document["components"]["schemas"]["WireguardRotation"]["properties"];
    assert_eq!(
        properties.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["publicKey"],
        "{document}"
    );
}

// The fail-closed guard, driven from the failing side: a `privateKey` planted
// in either tree comes back as the sentinel, and its value reaches no surface
// this daemon serves.

// The per-interface live-state fields reach the API surface: `kind` on every entry
// and `publicKey` on the tunnel, passed through untouched.
#[tokio::test]
pub(super) async fn the_state_route_serves_the_kind_and_the_public_key() {
    let (router, _, _cookie, token) = kinds_app().await;

    let response = bearer(&router, "GET", "/api/v1/state/network", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(body["eth0"]["kind"], json!("physical"));
    assert_eq!(body["eth0.100"]["kind"], json!("vlan"));
    assert_eq!(body["br0"]["kind"], json!("bridge"));
    assert_eq!(body["wg0"]["kind"], json!("wireguard"));
    assert_eq!(body["wg0"]["publicKey"], json!(PEER_KEY));
    // No entry carries a private key, because micad publishes none.
    for (name, entry) in body.as_object().unwrap() {
        assert!(
            entry.get("privateKey").is_none(),
            "{name} carries a private key: {entry}"
        );
    }
}

// `GET /api/v1/health` and the error envelope on a method a
// declared `/api/` route does not serve.
