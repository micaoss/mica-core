//! Reboot, power-off and the transient root password.

use crate::settings_api::SettingsApi;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use serde_json::json;

use super::*;

pub(super) const REBOOT_PATH: &str = "/api/v1/actions/reboot";
pub(super) const POWEROFF_PATH: &str = "/api/v1/actions/poweroff";
pub(super) const TRANSIENT_PATH: &str = "/api/v1/actions/transient-root-password";

#[tokio::test]
pub(super) async fn power_actions_report_admission_failures() {
    for (name, status, code) in [
        (
            Some("org.freedesktop.DBus.Error.AccessDenied"),
            StatusCode::CONFLICT,
            "power_refused",
        ),
        (
            Some("org.freedesktop.DBus.Error.Failed"),
            StatusCode::INTERNAL_SERVER_ERROR,
            "micad_failed",
        ),
        (None, StatusCode::SERVICE_UNAVAILABLE, "micad_unreachable"),
    ] {
        let (router, token) = failing_app(name).await;
        for path in [REBOOT_PATH, POWEROFF_PATH] {
            let response = bearer(&router, "POST", path, &token).await;
            assert_eq!(response.status(), status, "{path}: {name:?}");
            let error = envelope(response).await;
            assert_eq!(error["code"], code);
            if name.is_some() {
                assert_eq!(error["message"], MICAD_MESSAGE);
            }
        }
    }
}

#[tokio::test]
pub(super) async fn power_actions_report_unconfirmed_timeouts() {
    let (router, token) = timing_out_app().await;
    for path in [REBOOT_PATH, POWEROFF_PATH] {
        let response = bearer(&router, "POST", path, &token).await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(envelope(response).await["code"], "micad_timeout");
    }
}

#[tokio::test]
pub(super) async fn power_actions_accept_only_after_micad_dispatch() {
    let (tree, token) = with_token(configured_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    for (path, expected) in [(REBOOT_PATH, "reboot"), (POWEROFF_PATH, "power_off")] {
        let response = bearer(&router, "POST", path, &token).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            fake.power_calls().last().map(String::as_str),
            Some(expected)
        );
        assert!(body_string(response).await.is_empty());
    }
}

// The transient password reaches micad, is written into no setting, and is
// nowhere in the tree afterwards — the API half of the property the form path
// already holds.
#[tokio::test]
pub(super) async fn the_transient_password_route_sets_it_and_writes_no_setting() {
    const PASSWORD: &str = "correct horse battery";

    let (tree, token) = with_token(ssh_tree(json!([])));
    let (router, fake) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        TRANSIENT_PATH,
        &token,
        &json!({ "password": PASSWORD }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(header_value(&response, CACHE_CONTROL), "no-store");
    let accepted = body_json(response).await;
    assert!(accepted["taskId"].as_str().is_some(), "{accepted}");
    assert_eq!(fake.transient_password_calls(), 1);

    assert!(
        fake.set_paths().is_empty(),
        "a transient password must write no setting, got {:?}",
        fake.set_paths()
    );
    let tree = fake.get_settings("").await.unwrap().to_string();
    assert!(
        !tree.contains(PASSWORD),
        "the password must not appear in the settings tree"
    );
}

// **The same byte bounds as the form path, because it is the same function.**

// **A rejected password never appears in the response.**
#[tokio::test]
pub(super) async fn a_rejected_transient_password_is_never_echoed() {
    let (tree, token) = with_token(ssh_tree(json!([])));
    let (router, _) = test_app(tree);

    for password in [
        "shortpw",
        "quagga-vestibule-marzipan-cornice-thimble-quixotic-basalt-lantern-ferrule",
        "quagga\nvestibule",
    ] {
        let response = bearer_json(
            &router,
            "POST",
            TRANSIENT_PATH,
            &token,
            &json!({ "password": password }).to_string(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{password}"
        );
        let headers = format!("{:?}", response.headers());
        let body = body_string(response).await;
        assert!(!body.contains(password), "the body echoed it: {body}");
        assert!(!headers.contains(password), "a header echoed it: {headers}");
        // Nor a distinctive fragment of it: a truncated echo is still an echo.
        for fragment in ["quagga", "shortpw"] {
            if password.contains(fragment) {
                assert!(
                    !body.contains(fragment),
                    "the body echoed `{fragment}`: {body}"
                );
            }
        }
        // It is still a usable error envelope: the caller has to learn what the
        // rule was without being told what it sent. Every one of the
        // validator's messages names the subject and the rule and nothing else,
        // which is exactly why the message can be passed through verbatim.
        let error: serde_json::Value = serde_json::from_str(&body).expect("error envelope");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("Password must"),
            "{password}: {message}"
        );
    }
}

// A body that is not this shape is the `request_invalid` at 400, and the
// rejection text describes the shape rather than the value — so a malformed
// body carrying a password does not put it in the response either.
#[tokio::test]
pub(super) async fn a_malformed_transient_password_body_is_refused_at_400() {
    let (tree, token) = with_token(ssh_tree(json!([])));
    let (router, fake) = test_app(tree);

    for body in [
        "not json at all",
        r#"{"password": 7}"#,
        r#"{"passphrase": "hunter2secret"}"#,
        "{}",
    ] {
        let response = bearer_json(&router, "POST", TRANSIENT_PATH, &token, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_api_headers(&response, body);
        let error = envelope(response).await;
        assert_eq!(error["code"], "request_invalid", "{body}");
        assert_eq!(error["source"], "apid", "{body}");
    }
    assert_eq!(fake.transient_password_calls(), 0);
    assert!(fake.set_paths().is_empty());
}

// All three take a bearer token, a cookie or a bearer:
// `ApiSession`, so a cookie works too and the bearer is what a script uses.

// No credential, no action. The 401 is the error envelope and the machine stays
// up: this is the one route family where a missing check is unrecoverable.
#[tokio::test]
pub(super) async fn an_unauthenticated_action_post_is_refused_and_does_not_act() {
    for path in [REBOOT_PATH, POWEROFF_PATH, TRANSIENT_PATH] {
        let (router, fake) = test_app(ssh_tree(json!([])));
        let response = post_json(&router, path, r#"{"password":"hunter2secret"}"#, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_api_headers(&response, path);
        assert_eq!(
            envelope(response).await["code"],
            "not_authenticated",
            "{path}"
        );
        // Two calls' worth of deadline, then assert nothing arrived.
        assert!(
            fake.power_calls().is_empty(),
            "{path} acted without a credential"
        );
        assert_eq!(fake.transient_password_calls(), 0, "{path}");
    }
}

// A failed transient-password call is the error envelope with **no `path`
// member**: the route writes no setting, so there is no dot-path at fault.
#[tokio::test]
pub(super) async fn a_failed_transient_password_names_no_dot_path() {
    let (router, token) = failing_app(Some("org.freedesktop.DBus.Error.Failed")).await;

    let response = bearer_json(
        &router,
        "POST",
        TRANSIENT_PATH,
        &token,
        &json!({ "password": "hunter2secret" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_api_headers(&response, TRANSIENT_PATH);
    let error = envelope(response).await;
    assert_eq!(error["code"], "micad_failed");
    assert_eq!(error["source"], "micad");
    assert!(error.get("path").is_none(), "{error}");

    // A route that does name one still names it: the member is optional, not
    // removed. Same fixture, same failure, same classifier — the only
    // difference is that this one has a dot-path at fault.
    let response = bearer_json(
        &router,
        "PUT",
        "/api/v1/settings/hostname",
        &token,
        r#""mica""#,
    )
    .await;
    assert_eq!(envelope(response).await["path"], "hostname");
}

// The document describes all three, each `POST`-only: a documented `GET`
// would be a contract for a route that does not exist, and on these three
// paths it would be a contract to power the appliance off by following a link.
#[test]
pub(super) fn the_openapi_document_covers_the_three_actions() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    for (path, statuses) in [
        (REBOOT_PATH, ["202", "401", "405"].as_slice()),
        (POWEROFF_PATH, ["202", "401", "405"].as_slice()),
        (
            TRANSIENT_PATH,
            ["202", "400", "401", "405", "422", "500", "503", "504"].as_slice(),
        ),
    ] {
        let route = &document["paths"][path];
        assert!(route["post"].is_object(), "{path} is missing its POST");
        for status in statuses {
            assert!(
                route["post"]["responses"][status].is_object(),
                "{path} is missing its {status}: {document}"
            );
        }
        for method in ["get", "head", "put", "delete", "patch"] {
            assert!(
                route[method].is_null(),
                "{path} must declare no {method}: {document}"
            );
        }
    }

    // The one request body among the three carries exactly one member.
    let properties =
        &document["components"]["schemas"]["TransientRootPasswordRequest"]["properties"];
    assert_eq!(
        properties.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["password"],
        "{document}"
    );
}

// `POST /api/v1/setup`, the one unauthenticated write, and a behaviour fixed
// rather than documented.
