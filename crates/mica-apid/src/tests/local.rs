//! The local socket: a caller's uid decides what it may call.

use axum::extract::connect_info::ConnectInfo;
use axum::http::StatusCode;
use serde_json::json;

use crate::routes::{LocalPeer, local_app};

use super::*;

/// A device nobody has signed in to yet.
fn unclaimed() -> serde_json::Value {
    let mut tree = json!({ "hostname": "mica", "network": {}, "access": {} });
    tree["bluetooth"] = json!({ "enabled": true, "discoverable": false });
    tree
}

fn local_router(tree: serde_json::Value) -> (Router, Arc<FakeSettings>) {
    let fake = Arc::new(FakeSettings::new(tree));
    let state = AppState::new(fake.clone(), SIGNING_KEY);
    (local_app(state), fake)
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

// Any other uid, and a peer the kernel would not name, are refused
// everything.
#[tokio::test]
pub(super) async fn a_caller_other_than_root_is_refused_everything() {
    let (router, fake) = local_router(unclaimed());

    for uid in [Some(971), Some(1000), Some(65534), None] {
        for (method, path) in [
            ("GET", "/api/v1/network"),
            ("PUT", "/api/v1/settings/hostname"),
        ] {
            let response = local(&router, uid, method, path, Some(r#""edge-7""#)).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uid:?}");
            assert_eq!(envelope(response).await["code"], "not_allowed");
        }
    }
    assert!(fake.set_paths().is_empty());
}

// root, a command line on the device, may call the whole API with no
// credential.
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

    let response = local(&router, Some(0), "GET", "/api/v1/network", None).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// Only root can connect.
#[tokio::test]
pub(super) async fn the_socket_is_root_s_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("apid.sock");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

    let listener = crate::bind_local_socket(&path).expect("bound");
    assert_eq!(mode(&path), 0o600);
    drop(listener);

    // Binding again replaces the socket the first bind left.
    let _listener = crate::bind_local_socket(&path).expect("bound again");
    assert_eq!(mode(&path), 0o600);
}

/// Root on the device is already where `micad web network` runs, so the SSH
/// rule for leaving the network does not hold it.
#[tokio::test]
pub(super) async fn root_may_take_the_api_off_the_network_without_ssh() {
    let (router, fake) = local_router(unclaimed());
    let response = local(
        &router,
        Some(0),
        "PUT",
        "/api/v1/web",
        Some(r#"{"mode":"off","httpPort":8080,"httpsEnabled":false,"httpsPort":8443}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.get_settings("access.web").await.unwrap()["mode"],
        json!("off")
    );
}
