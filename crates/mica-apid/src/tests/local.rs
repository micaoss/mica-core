//! The local socket: a caller's uid decides what it may call.

use axum::extract::connect_info::ConnectInfo;
use axum::http::StatusCode;
use serde_json::json;

use crate::routes::{LocalPeer, LocalUsers, local_app};

use super::*;

const BLED_UID: u32 = 971;

/// A device nobody has signed in to yet, which is when a phone sets it up.
fn unclaimed() -> serde_json::Value {
    let mut tree = json!({ "hostname": "mica", "network": {}, "access": {} });
    tree["bluetooth"] = json!({ "enabled": true, "discoverable": false });
    tree
}

fn local_router(tree: serde_json::Value) -> (Router, Arc<FakeSettings>) {
    let fake = Arc::new(FakeSettings::new(tree));
    let state = AppState::new(fake.clone(), SIGNING_KEY);
    let users = LocalUsers {
        bled: Some(BLED_UID),
        bled_group: Some(BLED_UID),
    };
    (local_app(state, users), fake)
}

/// One request on the local socket from `uid`.
async fn local(
    router: &Router,
    uid: Option<u32>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo(LocalPeer { uid }));
    if body.is_some() {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    router
        .clone()
        .oneshot(
            request
                .body(Body::from(body.unwrap_or_default().to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

// mica-bled reads and writes the PIN and reads the network with no
// credential, before anybody has signed in: its uid is what lets it.
#[tokio::test]
pub(super) async fn mica_bled_calls_its_routes_without_a_credential() {
    let (router, fake) = local_router(unclaimed());

    let status = local(&router, Some(BLED_UID), "GET", "/api/v1/network", None).await;
    assert_eq!(status.status(), StatusCode::OK);

    let pin = local(
        &router,
        Some(BLED_UID),
        "PUT",
        "/api/v1/settings/bluetooth.pin",
        Some(r#""421137""#),
    )
    .await;
    assert_eq!(pin.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec!["bluetooth.pin"]);

    // The default code is refused here too: only a reset returns to it.
    let default = local(
        &router,
        Some(BLED_UID),
        "PUT",
        "/api/v1/settings/bluetooth.pin",
        Some(r#""000000""#),
    )
    .await;
    assert_eq!(default.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fake.set_paths(), vec!["bluetooth.pin"]);
}

// Nothing outside the list, however it is spelled, and not the console.
#[tokio::test]
pub(super) async fn mica_bled_calls_nothing_else() {
    let (router, fake) = local_router(unclaimed());

    for (method, path) in [
        ("GET", "/api/v1/settings/access"),
        ("PUT", "/api/v1/settings/hostname"),
        ("POST", "/api/v1/power/reboot"),
        ("POST", "/api/v1/network/status"),
        ("GET", "/api/v1/network/eth0/peers"),
        ("PUT", "/api/v1/network/.."),
        ("PUT", "/api/v1/network/"),
        ("PUT", "/api/v1/bluetooth"),
        ("GET", "/api/v1/tokens"),
        ("GET", "/_ui/"),
        ("GET", "/healthz"),
    ] {
        let response = local(&router, Some(BLED_UID), method, path, Some("true")).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(
            envelope(response).await["code"],
            "not_allowed",
            "{method} {path}"
        );
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// Any other uid, and a peer the kernel would not name, have no scope:
// refused everything.
#[tokio::test]
pub(super) async fn a_caller_without_a_scope_is_refused_everything() {
    let (router, _fake) = local_router(unclaimed());

    for uid in [Some(1000), Some(65534), None] {
        let response = local(&router, uid, "GET", "/api/v1/network", None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uid:?}");
    }
}

// root, a command line on the device, may call the whole API with no
// credential, including what mica-bled may not.
#[tokio::test]
pub(super) async fn root_calls_the_whole_api() {
    let (router, fake) = local_router(unclaimed());

    for path in [
        "/api/v1/settings/access",
        "/api/v1/tokens",
        "/api/v1/network",
    ] {
        let response = local(&router, Some(0), "GET", path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let hostname = local(
        &router,
        Some(0),
        "PUT",
        "/api/v1/settings/hostname",
        Some(r#""edge-7""#),
    )
    .await;
    assert_eq!(hostname.status(), StatusCode::ACCEPTED);
    assert_eq!(fake.set_paths(), vec!["hostname"]);
}

// The local credential exists only behind the local gate: on the network the
// same request needs a credential of its own.
#[tokio::test]
pub(super) async fn the_network_listener_grants_nothing_for_a_local_peer() {
    let (router, _fake) = test_app(unclaimed());

    let response = local(&router, Some(BLED_UID), "GET", "/api/v1/network", None).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// The ids come from the root's passwd and group, by name.
#[test]
pub(super) fn the_bled_ids_are_read_from_passwd_and_group() {
    let users = LocalUsers::from_files(
        "root:x:0:0:root:/root:/bin/sh\nmica-bled:x:971:971::/var/lib/mica-bled:/usr/sbin/nologin\n",
        "root:x:0:\nmica-bled:x:972:\n",
    );
    assert_eq!(users.bled, Some(BLED_UID));
    assert_eq!(users.bled_group, Some(972));
    let bare = LocalUsers::from_files("root:x:0:0::/:/bin/sh\n", "root:x:0:\n");
    assert_eq!((bare.bled, bare.bled_group), (None, None));
}

// Only root and mica-bled's group can connect: 0660 with the group, 0600
// without one.
#[tokio::test]
pub(super) async fn the_socket_is_root_s_and_mica_bled_s_alone() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("apid.sock");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

    let listener = crate::bind_local_socket(&path, None).expect("bound");
    assert_eq!(mode(&path), 0o600);
    drop(listener);

    // The group this test runs as is one it may give the socket to; binding
    // again replaces the socket the first bind left.
    let gid = std::fs::metadata(dir.path()).unwrap().gid();
    let _listener = crate::bind_local_socket(&path, Some(gid)).expect("bound again");
    assert_eq!(mode(&path), 0o660);
    assert_eq!(std::fs::metadata(&path).unwrap().gid(), gid);
}
