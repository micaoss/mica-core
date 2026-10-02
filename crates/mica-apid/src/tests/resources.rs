//! The settings and state roots: reads, redaction and observations.

use crate::auth;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

use super::*;

// A tree in the shape the inventory describes, carrying every one of the
// four redacted field names — at three depths and inside an array — so a walk
// over the responses below proves the denylist covers all of them.
pub(super) fn secret_tree(password: &str) -> serde_json::Value {
    json!({
        "hostname": "mica",
        // The field the denylist's fail-closed entry exists for. No shipped
        // schema has it — `WireguardConfig` carries no private key and never
        // will — so the fixture plants the hypothetical the entry guards
        // against: a settings tree that somehow holds one must not serve it.
        "network": {
            "wg0": { "kind": "wireguard", "privateKey": "wg-plaintext-marker" },
        },
        "access": {
            "webAdmin": { "password_hash": auth::hash_password(password).unwrap() },
            "device": { "passwordHash": "device-plaintext-marker" },
            "ssh": {
                "enabled": true,
                "authorizedKeys": [
                    { "comment": "laptop", "hash": "keyhash-plaintext-marker" },
                ],
            },
            // The one settings field that really is named `hash`: a bearer
            // token digest, inside an array, under the subtree the auth gate
            // reads on every request.
            "apiTokens": [
                {
                    "id": "3f2a9c41",
                    "name": "ci-deploy",
                    "hash": "token-digest-plaintext-marker",
                    "created": 1_700_000_000,
                },
            ],
        },
        "wifi": {
            "ap": { "ssid": "mica-ap", "psk": "ap-plaintext-marker" },
            "client": {
                "networks": [
                    { "ssid": "home", "psk": "home-plaintext-marker" },
                    {
                        "ssid": "work",
                        "psk": "work-plaintext-marker",
                        "extra": { "hash": "deep-plaintext-marker" },
                    },
                ],
            },
        },
    })
}

// The live-state entry the state tests read, carrying all five names too:
// the redaction rule for the settings root extends to the state root, so the state root is held to the same proof.
pub(super) fn secret_state_entry() -> serde_json::Value {
    json!({
        "psk": "state-ap-plaintext-marker",
        "privateKey": "state-private-plaintext-marker",
        "peers": [
            { "ssid": "home", "psk": "state-peer-plaintext-marker" },
            { "id": "laptop", "hash": "state-hash-plaintext-marker" },
        ],
        "admin": {
            "passwordHash": "state-camel-plaintext-marker",
            "nested": { "password_hash": "state-snake-plaintext-marker" },
        },
    })
}

// Every marker string [`secret_tree`] and [`secret_state_entry`] plant.
pub(super) const PLAINTEXT_MARKERS: [&str; 12] = [
    "token-digest-plaintext-marker",
    "wg-plaintext-marker",
    "state-private-plaintext-marker",
    "device-plaintext-marker",
    "keyhash-plaintext-marker",
    "ap-plaintext-marker",
    "home-plaintext-marker",
    "work-plaintext-marker",
    "deep-plaintext-marker",
    "state-ap-plaintext-marker",
    "state-peer-plaintext-marker",
    "state-hash-plaintext-marker",
];

// The field names the redaction rule names, `privateKey` included.
pub(super) const SECRET_FIELD_NAMES: [&str; 5] =
    ["psk", "passwordHash", "password_hash", "hash", "privateKey"];

// The sentinel a redacted field carries.
pub(super) const REDACTED: &str = "<redacted>";

// Collect every secret-bearing field in `value` — at any depth, inside arrays
// included — as `(name, value)` pairs.
pub(super) fn secret_fields(
    value: &serde_json::Value,
    found: &mut Vec<(String, serde_json::Value)>,
) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, child) in fields {
                if SECRET_FIELD_NAMES.contains(&name.as_str()) {
                    found.push((name.clone(), child.clone()));
                } else {
                    secret_fields(child, found);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                secret_fields(item, found);
            }
        }
        _ => {}
    }
}

// The dot-path IS the resource identifier, so the body is exactly what
// `GetSettings("<dot-path>")` returns.
#[tokio::test]
pub(super) async fn the_settings_root_answers_the_dot_paths_value_for_a_session() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    let response = bearer(&router, "GET", "/api/v1/settings/hostname", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/settings/hostname");
    assert_eq!(body_string(response).await, r#""mica""#);

    // A subtree, and a scalar reached through one: the passthrough has no
    // shape of its own to impose.
    let response = bearer(&router, "GET", "/api/v1/settings/access.ssh", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body_string(response).await).unwrap();
    assert_eq!(value["enabled"], json!(true));

    let response = bearer(
        &router,
        "GET",
        "/api/v1/settings/access.ssh.enabled",
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, "true");
}

// The second root, which is a different tree in micad and so a different route
// here: untyped, in memory, and written only from inside micad.
#[tokio::test]
pub(super) async fn the_state_root_answers_the_dot_paths_value_for_a_session() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("hostname", json!({ "applied": "mica" }));

    let response = bearer(&router, "GET", "/api/v1/state/hostname", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/state/hostname");
    assert_eq!(body_string(response).await, r#"{"applied":"mica"}"#);

    let response = bearer(&router, "GET", "/api/v1/state/hostname.applied", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response).await, r#""mica""#);
}

// The time-status surface: authenticated, read-only, and micad's
// classification passed through rather than re-derived here.
#[tokio::test]
pub(super) async fn the_time_status_route_answers_micads_classification_read_only() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_time_status(json!({
        "status": "offline-degraded",
        "synchronized": false,
    }));

    let response = bearer(&router, "GET", "/api/v1/time/status", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/time/status");
    let status = body_json(response).await;
    assert_eq!(status["status"], "offline-degraded");
    assert_eq!(status["synchronized"], json!(false));

    // Unauthenticated is 401 like every management read.
    let response = send(
        &router,
        Request::builder()
            .method("GET")
            .uri("/api/v1/time/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(envelope(response).await["code"], "not_authenticated");

    // Read-only: there is no verb here that could pause synchronization, so a
    // write is the method_not_allowed envelope, not a 404.
    let response = bearer_json(&router, "PUT", "/api/v1/time/status", &token, "{}").await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(envelope(response).await["code"], "method_not_allowed");
}

// The storage surface: authenticated, read-only, and micad's
// observation passed through rather than re-derived here.
#[tokio::test]
pub(super) async fn the_storage_status_route_answers_micads_observation_read_only() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_storage_status(json!({
        "tiers": [{
            "name": "data",
            "role": "ext4",
            "present": true,
            "mount": "/srv",
            "readOnly": false,
            "space": { "totalBytes": 1000, "usedBytes": 850, "freeBytes": 100, "reservedBytes": 50, "usedPercent": 85 },
            "pressure": "warning",
            "check": { "unit": "systemd-fsck@dev-mmcblk0p11.service", "result": "success", "exitStatus": 1 },
        }],
        "media": [{ "name": "nvme0n1", "kind": "nvme", "health": { "supported": false, "reason": "no SMART reader" } }],
        "policy": { "warningPercent": 80, "criticalPercent": 90 },
        "lifecycle": { "backupRestore": "unsupported", "encryption": "unsupported" },
    }));

    let response = bearer(&router, "GET", "/api/v1/storage/status", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_api_headers(&response, "/api/v1/storage/status");
    let status = body_json(response).await;
    assert_eq!(status["tiers"][0]["name"], "data");
    assert_eq!(status["tiers"][0]["pressure"], "warning");
    assert_eq!(status["tiers"][0]["space"]["reservedBytes"], 50);
    // An unsupported metric reaches the client as unsupported, not as an
    // omission that reads like health.
    assert_eq!(status["media"][0]["health"]["supported"], false);
    assert_eq!(status["lifecycle"]["encryption"], "unsupported");

    // Unauthenticated is 401 like every management read.
    let response = send(
        &router,
        Request::builder()
            .method("GET")
            .uri("/api/v1/storage/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(envelope(response).await["code"], "not_authenticated");

    // Read-only: no verb here could rewrite the layout, so a write is
    // the method_not_allowed envelope rather than a 404.
    for method in ["PUT", "POST", "DELETE", "PATCH"] {
        let response = bearer_json(&router, method, "/api/v1/storage/status", &token, "{}").await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} on the storage status"
        );
        assert_eq!(envelope(response).await["code"], "method_not_allowed");
    }
}

// The last acceptance bullet, as a gate rather than a promise: normal
// apid exposes NO generic format or repartition action.
#[tokio::test]
pub(super) async fn normal_apid_exposes_no_format_or_repartition_action() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");
    let paths = document["paths"].as_object().expect("paths is an object");

    // The search space is populated, and it contains the storage surface
    // this test is about: a scan over an empty or storage-less document
    // would pass forever while proving nothing.
    assert!(
        paths.len() > 20,
        "the document lists too few paths: {paths:?}"
    );
    assert!(
        paths.contains_key("/api/v1/storage/status"),
        "the storage surface is missing, so this scan is not scanning it: {paths:?}"
    );

    const FORBIDDEN: [&str; 12] = [
        "format",
        "repartition",
        "partition",
        "mkfs",
        "fdisk",
        "resize",
        "wipe",
        "erase",
        "lvm",
        "raid",
        // The status surface describes mounts, so the API must not grow
        // a verb that moves one: the units own the mounts, not apid.
        "mount",
        "unmount",
    ];
    for path in paths.keys() {
        let lowered = path.to_ascii_lowercase();
        for word in FORBIDDEN {
            assert!(
                !lowered.contains(word),
                "the API declares `{path}`, which names the `{word}` operation this product does not have"
            );
        }
    }

    // And the paths a client would guess are not served at all. Every method,
    // because a route that answered a POST while refusing a GET would still
    // be a destructive surface.
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _fake) = test_app(tree);
    const GUESSES: [&str; 8] = [
        "/api/v1/storage/format",
        "/api/v1/storage/repartition",
        "/api/v1/storage/partitions",
        "/api/v1/actions/format",
        "/api/v1/actions/factory-reset",
        "/api/v1/storage/wipe",
        "/api/v1/storage/mount",
        "/api/v1/storage/namespaces",
    ];
    for path in GUESSES {
        for method in ["GET", "POST", "PUT", "DELETE"] {
            let response = bearer_json(&router, method, path, &token, "{}").await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{method} {path} is served by something"
            );
            assert_eq!(
                envelope(response).await["code"],
                "not_found",
                "{method} {path}"
            );
        }
    }
}

// The two roots are separate: a settings dot-path is not a state dot-path,
// and the routes do not fall back to each other.
#[tokio::test]
pub(super) async fn the_two_roots_do_not_answer_for_each_other() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("hostname", json!({ "applied": "mica" }));

    // `hostname` exists in both, with different values.
    let settings = bearer(&router, "GET", "/api/v1/settings/hostname", &token).await;
    let state = bearer(&router, "GET", "/api/v1/state/hostname", &token).await;
    assert_ne!(
        body_string(settings).await,
        body_string(state).await,
        "one root answered for the other"
    );

    // `network` exists only in the settings tree, so the state root must fail
    // rather than serve the settings value.
    let response = bearer(&router, "GET", "/api/v1/state/network", &token).await;
    assert_ne!(response.status(), StatusCode::OK);
}

// The document describes the served surface: a client reading only
// `openapi.json` has to learn both families and every outcome they have.
#[test]
pub(super) fn the_openapi_document_covers_the_resource_routes() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    for path in ["/api/v1/settings/{path}", "/api/v1/state/{path}"] {
        let responses = &document["paths"][path]["get"]["responses"];
        for status in ["200", "401", "422", "500", "503"] {
            assert!(
                responses[status].is_object(),
                "{path} is missing its {status}: {document}"
            );
        }
    }

    // The two fixed-path status routes name no dot-path, so they carry no
    // 422; every other outcome a client has to handle is still declared.
    for path in ["/api/v1/time/status", "/api/v1/storage/status"] {
        let responses = &document["paths"][path]["get"]["responses"];
        for status in ["200", "401", "500", "503", "504"] {
            assert!(
                responses[status].is_object(),
                "{path} is missing its {status}: {document}"
            );
        }
        assert!(
            document["paths"][path].get("put").is_none()
                && document["paths"][path].get("post").is_none()
                && document["paths"][path].get("delete").is_none(),
            "{path} declares a write verb: {document}"
        );
    }

    // The sentinel is a value a client can receive, so the schema of the
    // body has to say so; a client that has not been told treats
    // `"<redacted>"` as the credential.
    assert!(
        document["components"]["schemas"]["ResourceValue"]["description"]
            .as_str()
            .is_some_and(|text| text.contains(REDACTED)),
        "the resource body's schema does not describe the redaction sentinel: {document}"
    );
}

// The redaction rule, driven from the failing side: every field the
// denylist names, at every depth the tree puts one and inside the arrays the
// dot-path syntax cannot address, comes back as the sentinel.
#[tokio::test]
pub(super) async fn every_redacted_field_name_comes_back_redacted_from_the_settings_root() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    // Three subtrees rather than one, because the whole-tree dot-path is `""`
    // and this route family takes a non-empty one. Between them they hold all
    // five names.
    let mut found = Vec::new();
    let mut bodies = String::new();
    for path in [
        "/api/v1/settings/access",
        "/api/v1/settings/wifi",
        "/api/v1/settings/network",
    ] {
        let response = bearer(&router, "GET", path, &token).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = body_string(response).await;
        secret_fields(&serde_json::from_str(&body).unwrap(), &mut found);
        bodies.push_str(&body);
    }

    let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    for name in SECRET_FIELD_NAMES {
        assert!(
            names.contains(&name),
            "the fixture no longer carries a `{name}` field, so this test does not cover it: {names:?}"
        );
    }
    for (name, value) in &found {
        assert_eq!(value, &json!(REDACTED), "`{name}` was served in the clear");
    }
    // The walk only sees fields it recognises. This sees the bytes.
    for marker in PLAINTEXT_MARKERS {
        assert!(
            !bodies.contains(marker),
            "`{marker}` reached the wire: {bodies}"
        );
    }
}

// A settings read of `access` never carries a token digest.
#[tokio::test]
pub(super) async fn a_settings_read_of_access_never_carries_a_token_digest() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, _) = test_app(tree);

    // The subtree the gate reads.
    let response = bearer(&router, "GET", "/api/v1/settings/access", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        !body.contains("token-digest-plaintext-marker"),
        "a token digest reached the wire: {body}"
    );

    // The entry is still served -- the id, the name and the clock reading are
    // what `GET /api/v1/tokens` lists -- so this is redaction and not removal.
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entry = &value["apiTokens"][0];
    assert_eq!(entry["id"], json!("3f2a9c41"));
    assert_eq!(entry["name"], json!("ci-deploy"));
    assert_eq!(entry["created"], json!(1_700_000_000));
    assert_eq!(entry["hash"], json!(REDACTED));

    // The array on its own, which is the walk's array branch with nothing
    // above it to have caught the field first.
    let response = bearer(&router, "GET", "/api/v1/settings/access.apiTokens", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        !body.contains("token-digest-plaintext-marker"),
        "the token array served the digest: {body}"
    );

    // There is no dot-path that reaches one entry: the syntax has no array
    // indexing, which is why the denylist is by field name and not by path.
    let response = bearer(
        &router,
        "GET",
        "/api/v1/settings/access.apiTokens.0.hash",
        &token,
    )
    .await;
    let status = response.status();
    assert_ne!(
        status,
        StatusCode::OK,
        "an indexed dot-path resolved: {}",
        body_string(response).await
    );
}

// The same rule on the state root, not only the settings root, because a denylist that covers one root while the
// other serves the same field names verbatim is a hole with a tested-looking
// lid.
#[tokio::test]
pub(super) async fn the_state_root_is_redacted_by_the_same_rule() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("wifiAp", secret_state_entry());

    let response = bearer(&router, "GET", "/api/v1/state/wifiAp", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;

    let mut found = Vec::new();
    secret_fields(&serde_json::from_str(&body).unwrap(), &mut found);
    let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    for name in SECRET_FIELD_NAMES {
        assert!(names.contains(&name), "not covered: {name} in {names:?}");
    }
    for (name, value) in &found {
        assert_eq!(value, &json!(REDACTED), "`{name}` was served in the clear");
    }
    for marker in PLAINTEXT_MARKERS {
        assert!(
            !body.contains(marker),
            "`{marker}` reached the wire: {body}"
        );
    }
}

// The structural walk keys on a field name, and a dot-path that names a
// secret field directly leaves no field name in the value: the response is
// the bare hash. So the requested path is redacted as well as the tree.
#[tokio::test]
pub(super) async fn a_dot_path_that_names_a_secret_field_answers_the_sentinel() {
    let (tree, token) = with_token(secret_tree("hunter2secret"));
    let (router, fake) = test_app(tree);
    fake.set_state_entry("wifiAp", secret_state_entry());

    for path in [
        "/api/v1/settings/access.webAdmin.password_hash",
        "/api/v1/settings/access.device.passwordHash",
        "/api/v1/settings/wifi.ap.psk",
        "/api/v1/settings/network.wg0.privateKey",
        "/api/v1/state/wifiAp.psk",
        "/api/v1/state/wifiAp.privateKey",
        "/api/v1/state/wifiAp.admin.passwordHash",
    ] {
        let response = bearer(&router, "GET", path, &token).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            body_string(response).await,
            format!(r#""{REDACTED}""#),
            "{path}"
        );
    }
}
