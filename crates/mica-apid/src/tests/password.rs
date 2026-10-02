//! The administrator password change.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{Request, Response, StatusCode};

use super::*;

// POST a JSON body, the shape the API's one write route takes.
pub(super) async fn post_json(
    router: &Router,
    path: &str,
    body: &str,
    cookie: Option<&str>,
) -> Response<axum::body::Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(COOKIE, format!("apid_session={cookie}"));
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

// A wrong current password writes nothing and the old credential stands.

// The decided semantics, end to end: the new hash lands in the settings
// tree, every other session is invalidated, and the acting session survives.

// A mismatched confirmation is refused before the current password is even
// looked at, in the same shape as the setup wizard's refusal.

// The API half of the same refusal: the error envelope, `wrong_password`, and
// nothing written.
#[tokio::test]
pub(super) async fn the_api_password_change_rejects_a_wrong_current_password() {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/actions/change-password",
        &token,
        r#"{"currentPassword":"not-the-password","newPassword":"newsecret9"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_api_headers(&response, "/api/v1/actions/change-password");
    let error = envelope(response).await;
    assert_eq!(error["code"], "wrong_password");
    assert_eq!(error["source"], "apid");
    assert!(fake.set_paths().is_empty());
}

// The API half of the success: 204, the hash written, and **every** browser
// session dropped.

// A new password under eight characters is refused with `validation_failed`,
// the same floor the setup wizard enforces.
#[tokio::test]
pub(super) async fn the_api_password_change_rejects_a_short_new_password() {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/actions/change-password",
        &token,
        r#"{"currentPassword":"hunter2secret","newPassword":"short"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "validation_failed");
    assert!(fake.set_paths().is_empty());
}

// The trap, held for the one write route: unauthenticated is the 401
// envelope in both gate modes, never a redirect a script reads as success.
#[tokio::test]
pub(super) async fn the_api_password_change_is_401_without_a_session_in_both_gate_modes() {
    let (configured, _) = test_app(configured_tree("hunter2secret"));
    let (fresh, _) = test_app(unconfigured_tree());

    for (mode, router) in [("configured", &configured), ("setup mode", &fresh)] {
        let response = post_json(
            router,
            "/api/v1/actions/change-password",
            r#"{"currentPassword":"hunter2secret","newPassword":"newsecret9"}"#,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{mode}");
        assert_eq!(response.headers().get(LOCATION), None, "{mode}");
        let error = envelope(response).await;
        assert_eq!(error["code"], "not_authenticated", "{mode}");
    }
}

// A body that is not the declared shape answers the error envelope rather than
// axum's plain-text rejection.
#[tokio::test]
pub(super) async fn the_api_password_change_rejects_a_malformed_body_with_the_envelope() {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/actions/change-password",
        &token,
        r#"{"currentPassword":"hunter2secret"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_api_headers(&response, "/api/v1/actions/change-password");
    let error = envelope(response).await;
    assert_eq!(error["code"], "request_invalid");
    assert_eq!(error["source"], "apid");
    assert!(fake.set_paths().is_empty());
}

// The gate's cache of the `access` subtree.

// The lockout rule at the route level: with no live subscription every
// unauthenticated request reads the bus — the pre-cache behaviour, and the
// fallback the rule demands; with one, the first request fills the cache and
// the rest are served from it; an invalidation forces exactly one re-read;
// a lapse falls all the way back to direct reads.

// The password change against the cache — the sequence named as
// the hard case, made real by the change-password route: the flow itself
// verifies against the bus even while the cache is primed, its write drops
// the cached snapshot without waiting for the `SettingsChanged` round trip,
// and the next unauthenticated request re-reads and observes the
// post-change tree.

// Completing setup IS the setup-mode decision changing under the gate — the
// exact decision the cache must never serve stale. The wizard's
// `access.webAdmin` write drops the cache, so the next unauthenticated
// request re-reads and redirects to `/login`, not back into `/setup`.

// The typed network pane, the rotate-key route, and the two
// per-interface live-state fields.
