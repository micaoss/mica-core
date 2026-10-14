//! The settings write route.

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL};
use axum::http::{Request, Response, StatusCode};
use serde_json::json;

use super::*;

// A tree with all writable paths already present, so a write is a change
// of value and never a creation -- the creation case is what the refusal list
// exists to prevent, and it must not be smuggled into the happy path.
pub(super) fn writable_tree(password: &str) -> serde_json::Value {
    let mut tree = configured_tree(password);
    tree["access"]["ssh"] = json!({ "enabled": false });
    tree["container"] = json!({ "enabled": false });
    tree["mqtt"] = json!({ "enabled": false });
    tree["wifi"] = serde_json::to_value(micad_settings::WifiSettings::default()).unwrap();
    tree["time"] = json!({ "ntp": { "servers": [] }, "timezone": "UTC" });
    tree
}

// The seven dot-paths admitted, each written and
// each read back through the route that answers for it.
//
// 202 and a task id: persistence has completed, while reconciliation is a
// separately observable lifecycle.
#[tokio::test]
pub(super) async fn the_write_route_writes_the_scalar_settings() {
    let (tree, token) = with_token(writable_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    for (path, body) in [
        ("hostname", r#""router7""#),
        ("access.ssh.enabled", "true"),
        ("container.enabled", "true"),
        ("mqtt.enabled", "true"),
        ("wifi.client.enabled", "true"),
        ("wifi.client.enabled", "false"),
        ("time.ntp.servers", r#"["0.pool.ntp.org","192.0.2.7"]"#),
        ("time.timezone", r#""Europe/Berlin""#),
    ] {
        let url = format!("/api/v1/settings/{path}");
        let response = bearer_json(&router, "PUT", &url, &token, body).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{path}");
        assert_eq!(header_value(&response, CACHE_CONTROL), "no-store", "{path}");
        let accepted = body_json(response).await;
        let task_id = accepted["taskId"].as_str().expect("a task id");

        let task = bearer(&router, "GET", &format!("/api/v1/tasks/{task_id}"), &token).await;
        assert_eq!(task.status(), StatusCode::OK, "{path}");
        let task = body_json(task).await;
        assert_eq!(task["dotPath"], path, "{task}");
        assert_eq!(task["status"], "finished", "{task}");
        assert_eq!(task["outcome"], "succeeded", "{task}");

        let read = bearer(&router, "GET", &url, &token).await;
        assert_eq!(read.status(), StatusCode::OK, "{path}");
        assert_eq!(
            body_string(read).await,
            body,
            "{path} reads back as written"
        );
    }

    assert_eq!(
        fake.set_paths(),
        vec![
            "hostname",
            "access.ssh.enabled",
            "container.enabled",
            "mqtt.enabled",
            "wifi.client.enabled",
            "wifi.client.enabled",
            "time.ntp.servers",
            "time.timezone"
        ],
        "one bus write per request, at the dot-path the URL named"
    );

    let tasks = bearer(&router, "GET", "/api/v1/tasks", &token).await;
    assert_eq!(tasks.status(), StatusCode::OK);
    assert_eq!(body_json(tasks).await.as_array().unwrap().len(), 8);

    let missing = bearer(&router, "GET", "/api/v1/tasks/not-retained", &token).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(envelope(missing).await["code"], "task_not_found");
}

#[tokio::test]
pub(super) async fn the_browser_wifi_switch_requires_a_session_and_csrf() {
    let tree = writable_tree("hunter2secret");
    let original_wifi = tree["wifi"].clone();
    let (router, fake) = test_app(tree);
    let url = "/api/v1/settings/wifi.client.enabled";

    let unauthenticated = json_request(&router, "PUT", url, json!(true), None, None).await;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(envelope(unauthenticated).await["code"], "not_authenticated");

    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    let cookie = session_cookie_value(&login);
    let session = body_json(login).await;
    let csrf = session["csrfToken"].as_str().unwrap();

    for invalid_csrf in [None, Some("wrong-token")] {
        let response = json_request(
            &router,
            "PUT",
            url,
            json!(true),
            Some(&cookie),
            invalid_csrf,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(envelope(response).await["code"], "csrf_invalid");
    }
    assert!(fake.set_paths().is_empty());

    for enabled in [true, false] {
        let response = json_request(
            &router,
            "PUT",
            url,
            json!(enabled),
            Some(&cookie),
            Some(csrf),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(body_json(response).await["taskId"].is_string());
        let read = get(&router, "/api/v1/settings/wifi", Some(&cookie)).await;
        assert_eq!(read.status(), StatusCode::OK);
        let mut expected = original_wifi.clone();
        expected["client"]["enabled"] = json!(enabled);
        assert_eq!(body_json(read).await, expected);
    }
    assert_eq!(fake.set_paths(), vec!["wifi.client.enabled"; 2]);
}

// The round trip, driven exactly as the client that motivates the rule
// would drive it: read a subtree, hand it back, and find the credential
// intact rather than replaced by the sentinel.
#[tokio::test]
pub(super) async fn a_write_carrying_the_redaction_sentinel_is_refused_and_writes_nothing() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    // The exact bytes a client would have read, sentinels and all.
    let read = bearer(&router, "GET", "/api/v1/settings/access", &token).await;
    assert_eq!(read.status(), StatusCode::OK);
    let redacted = body_string(read).await;
    assert!(
        redacted.contains(REDACTED),
        "the fixture must carry a redacted field: {redacted}"
    );

    let response = bearer_json(&router, "PUT", "/api/v1/settings/access", &token, &redacted).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_api_headers(&response, "the sentinel refusal");
    let error = envelope(response).await;
    assert_eq!(error["code"], "validation_failed");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!("access"));
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|text| text.contains(REDACTED)),
        "the message has to name what it refused: {error}"
    );

    // Nothing reached the bus, and the credential the sentinel stood for still
    // verifies -- which is the whole of what this rule protects.
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert!(
        !login(&router, "hunter2secret").await.is_empty(),
        "the admin hash must still be the hash"
    );

    // The same rule on an allowlisted path, where the sentinel is the whole
    // body rather than a field inside one: a client that read
    // `access.webAdmin.password_hash` got a bare `"<redacted>"` string back.
    let response = bearer_json(
        &router,
        "PUT",
        "/api/v1/settings/hostname",
        &token,
        &format!("\"{REDACTED}\""),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "validation_failed");
    assert!(fake.set_paths().is_empty());
}

// The refusal list: a dot-path the schema has and this route does not write
// is **409 `settings_read_only`**, answered before any bus call.
#[tokio::test]
pub(super) async fn every_dot_path_outside_the_allowlist_is_refused_with_409() {
    let (tree, token) = with_token(writable_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    for path in [
        "network",
        "network.eth0",
        "network.eth0.dhcp",
        "access",
        "access.ssh",
        "access.ssh.authorizedKeys",
        "access.webAdmin.password_hash",
        "provisioning",
        "wifi",
        "wifi.client",
        "wifi.client.interface",
        "wifi.client.networks",
        "wifi.ap",
        "wifi.ap.mode",
        "container",
        "mqtt",
        "mqtt.listen.port",
        "time",
        "time.ntp",
        // `.` is the whole tree, not a malformed path: `Settings::set`
        // documents `""` and `"."` as replacing the root, so it is a real path
        // this route refuses rather than one it cannot parse.
        ".",
    ] {
        let response = bearer_json(
            &router,
            "PUT",
            &format!("/api/v1/settings/{path}"),
            &token,
            "true",
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
        assert_api_headers(&response, path);
        let error = envelope(response).await;
        assert_eq!(error["code"], "settings_read_only", "{path}");
        assert_eq!(error["source"], "apid", "{path}");
        assert_eq!(error["path"], json!(path), "{path}");
    }

    assert!(
        fake.set_paths().is_empty(),
        "a refused write must reach no bus call, got {:?}",
        fake.set_paths()
    );
}

// The named refusal carries a message the general one cannot, and it is
// asserted because it is the reason the path is refused rather than
// decoration.
#[tokio::test]
pub(super) async fn the_named_refusal_says_why_rather_than_only_that() {
    let (tree, token) = with_token(writable_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    // `network` names the typed route that owns it, because a raw write here
    // creates an entry of the default kind rather than refusing an interface
    // the device does not have.
    let response = bearer_json(
        &router,
        "PUT",
        "/api/v1/settings/network.wg9",
        &token,
        r#"{"dhcp": true}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let message = envelope(response).await["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        message.contains("PUT /api/v1/network/{iface}"),
        "the refusal must name the route that does own it: {message}"
    );
}

// The rule, on the write route: **well-formed but absent is 404, not
// well-formed is 422**, and they must not share a status.
#[tokio::test]
pub(super) async fn an_absent_root_is_404_and_a_malformed_path_is_422() {
    let (tree, token) = with_token(writable_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    for path in [
        "hostnam",
        "netwrok.eth0",
        "acess.ssh.enabled",
        "sshd",
        // It is a root the typed schema has no field for, which is
        // exactly what this test is about.
        "schema_version",
    ] {
        let response = bearer_json(
            &router,
            "PUT",
            &format!("/api/v1/settings/{path}"),
            &token,
            "true",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let error = envelope(response).await;
        assert_eq!(error["code"], "settings_not_found", "{path}");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|text| text.contains(path)),
            "{path}: {error}"
        );
    }

    for path in [
        "access..ssh",
        "access.\"ssh",
        "access.\"ssh\"x",
        "hostname.",
    ] {
        let response = bearer_json(
            &router,
            "PUT",
            &format!("/api/v1/settings/{path}"),
            &token,
            "true",
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed", "{path}");
    }

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The nine top-level keys the write route's not-found rule is derived from.
#[test]
pub(super) fn the_settings_schema_has_the_nine_roots_the_write_route_knows() {
    let tree = serde_json::to_value(micad_settings::Settings::default()).unwrap();
    let mut keys: Vec<&str> = tree
        .as_object()
        .expect("the settings tree is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "access",
            "bluetooth",
            "container",
            "hostname",
            "mqtt",
            "network",
            "provisioning",
            "time",
            "wifi",
        ]
    );
}

// Each writable path's value has one shape, and a body of the wrong shape is
// a 422 that names the shape rather than a write of whatever arrived.
//
// The hostname sentence is `HOSTNAME_RULES`, the same string the form pane
// puts in its error box: one rule, one wording, two surfaces.
#[tokio::test]
pub(super) async fn a_body_of_the_wrong_shape_is_refused_and_not_written() {
    let (tree, token) = with_token(writable_tree("hunter2secret"));
    let (router, fake) = test_app(tree);

    for (path, body, expected) in [
        ("hostname", "true", "text"),
        ("hostname", "7", "text"),
        ("hostname", r#"["a"]"#, "text"),
        ("hostname", r#""-nope-""#, "hyphen"),
        ("hostname", r#""""#, "1-63"),
        ("hostname", r#""has space""#, "1-63"),
        ("access.ssh.enabled", r#""yes""#, "switch"),
        ("container.enabled", "1", "switch"),
        ("mqtt.enabled", "null", "switch"),
        ("wifi.client.enabled", r#""true""#, "switch"),
        ("wifi.client.enabled", "1", "switch"),
        ("wifi.client.enabled", "null", "switch"),
        ("wifi.client.enabled", "{}", "switch"),
        ("wifi.client.enabled", "[]", "switch"),
        ("time.timezone", "true", "text"),
        ("time.timezone", r#""Not A Zone!""#, "IANA"),
        ("time.timezone", r#""Etc//UTC""#, "IANA"),
        ("time.ntp.servers", r#""0.pool.ntp.org""#, "list"),
        ("time.ntp.servers", "[7]", "list"),
        ("time.ntp.servers", r#"["bad server"]"#, "host name"),
        ("time.ntp.servers", r#"["a.example","a.example"]"#, "twice"),
    ] {
        let response = bearer_json(
            &router,
            "PUT",
            &format!("/api/v1/settings/{path}"),
            &token,
            body,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path} <- {body}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed", "{path} <- {body}");
        assert_eq!(error["path"], json!(path), "{path} <- {body}");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|text| text.contains(expected)),
            "{path} <- {body}: {error}"
        );
    }

    // Not JSON at all is 400 and not 422: the request never became a value to
    // validate. Same classification the mint route gives the same condition.
    let response = bearer_json(
        &router,
        "PUT",
        "/api/v1/settings/hostname",
        &token,
        "router7",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(envelope(response).await["code"], "request_invalid");

    // A body with no `Content-Type: application/json` is the same refusal.
    let response = send(
        &router,
        Request::builder()
            .method("PUT")
            .uri("/api/v1/settings/hostname")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(r#""router7""#))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(envelope(response).await["code"], "request_invalid");

    assert!(
        fake.set_paths().is_empty(),
        "a refused write must write nothing, got {:?}",
        fake.set_paths()
    );
}

// The credential: bearer **or** cookie, which is the
// ruling applied to a new route. The bearer-only rule is about the token
// routes specifically, so this route matches the shipped reads instead.

// A write micad refuses is classified by the table exactly as a read is:
// the route adds no second opinion, and micad's own message comes through.
#[tokio::test]
pub(super) async fn a_write_micad_refuses_carries_micads_classification() {
    for (fdo_name, code, status) in [
        (
            "org.freedesktop.DBus.Error.InvalidArgs",
            "settings_rejected",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "com.mica.micad1.Error.ReadOnly",
            "settings_read_only",
            StatusCode::CONFLICT,
        ),
        (
            "org.freedesktop.DBus.Error.IOError",
            "settings_io",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        let (router, token) = failing_app(Some(fdo_name)).await;
        let response = bearer_json(
            &router,
            "PUT",
            "/api/v1/settings/hostname",
            &token,
            r#""router7""#,
        )
        .await;
        assert_eq!(response.status(), status, "{fdo_name}");
        let error = envelope(response).await;
        assert_eq!(error["code"], code, "{fdo_name}");
        assert_eq!(error["source"], "micad", "{fdo_name}");
        assert_eq!(error["message"], MICAD_MESSAGE, "{fdo_name}");
        assert_eq!(error["path"], json!("hostname"), "{fdo_name}");
    }
}

// The document describes the served surface: a client reading only
// `openapi.json` has to learn the write route, every outcome it has, and that
// its body is a bare JSON value.
#[test]
pub(super) fn the_openapi_document_covers_the_settings_write() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let write = &document["paths"]["/api/v1/settings/{path}"]["put"];
    for status in [
        "202", "400", "401", "404", "405", "409", "422", "500", "503", "504",
    ] {
        assert!(
            write["responses"][status].is_object(),
            "the settings write must document {status}: {write}"
        );
    }
    assert_eq!(
        write["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/SettingsWrite",
        "{write}"
    );

    // The read is unchanged by the write sharing its path.
    assert!(
        document["paths"]["/api/v1/settings/{path}"]["get"]["responses"]["200"].is_object(),
        "{document}"
    );

    let tasks = &document["paths"]["/api/v1/tasks"]["get"];
    assert!(tasks["responses"]["200"].is_object(), "{tasks}");
    let task = &document["paths"]["/api/v1/tasks/{id}"]["get"];
    for status in ["200", "401", "404", "405", "500", "503", "504"] {
        assert!(task["responses"][status].is_object(), "{status}: {task}");
    }
}

// The two array collections that already
// exist in the settings tree -- the SSH authorized keys, identified by
// fingerprint, and the WiFi station's known networks, identified by SSID.

// The whole body as JSON, for the collection routes that answer a document
// rather than the error envelope.
pub(super) async fn body_json(response: Response<axum::body::Body>) -> serde_json::Value {
    let body = body_string(response).await;
    serde_json::from_str(&body).unwrap_or_else(|_| panic!("a JSON body, got: {body}"))
}
