//! The system log route: one allowlisted service's lines, scrubbed.

use axum::http::StatusCode;

use super::*;

#[tokio::test]
pub(super) async fn a_service_log_is_served_scrubbed_and_an_unknown_one_is_refused() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    let response = bearer(&router, "GET", "/api/v1/system/logs/micad", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/system/logs/micad");
    let log = body_json(response).await;
    assert_eq!(log["source"], "micad");
    assert_eq!(
        log["lines"][0],
        "2026-09-28T10:00:00+00:00 micad[1]: serving"
    );
    // A line carrying a secret marker is replaced whole; a hardware address
    // is masked in place.
    let text = log.to_string();
    assert!(!text.contains("hunter22"), "{text}");
    // The device code is the claimer's proof: an old line carrying it is
    // replaced whole too.
    assert!(!text.contains("DFN4-L34C-T8N"), "{text}");
    assert!(!text.contains("aa:bb:cc:dd:ee:ff"), "{text}");
    assert!(log["lines"][2].as_str().unwrap().contains("link"), "{log}");

    let response = bearer(&router, "GET", "/api/v1/system/logs/kernel", &token).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(
        super::diagnostics::unauthenticated(&router, "GET", "/api/v1/system/logs/micad").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
pub(super) async fn the_previous_boot_is_read_scrubbed_or_absent_with_the_reason() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    let response = bearer(
        &router,
        "GET",
        "/api/v1/system/logs/micad?boot=previous",
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let log = body_json(response).await;
    assert_eq!(
        log["lines"][0],
        "2026-09-28T09:59:00+00:00 micad[1]: stopping"
    );
    assert!(!log.to_string().contains("hunter22"), "{log}");

    let response = bearer(
        &router,
        "GET",
        "/api/v1/system/logs/apid?boot=previous",
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let log = body_json(response).await;
    assert_eq!(log["available"], false);
    assert!(
        log["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no previous boot is kept")),
        "{log}"
    );

    let response = bearer(
        &router,
        "GET",
        "/api/v1/system/logs/micad?boot=current",
        &token,
    )
    .await;
    assert_eq!(
        body_json(response).await["lines"][0],
        "2026-09-28T10:00:00+00:00 micad[1]: serving"
    );

    let response = bearer(&router, "GET", "/api/v1/system/logs/micad?boot=-1", &token).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "validation_failed");
}
