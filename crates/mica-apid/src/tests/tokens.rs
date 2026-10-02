//! API tokens.

use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, Response, StatusCode};
use serde_json::json;

use super::*;

// A stored entry and the plaintext that opens it, both derived from `index`
// so two calls differ in every field identity is keyed on.
//
// Built here rather than minted, because a test that needs a full list needs
// 32 of them and the mint is one of the things under test.
pub(super) fn seeded_token(index: usize) -> (serde_json::Value, String) {
    let id = format!("{index:08x}");
    let secret = format!("{index:064x}");
    (
        json!({
            "id": id,
            "name": format!("seeded-{index}"),
            "hash": crate::token::digest(&secret),
            "created": 1,
        }),
        format!("mica_{id}_{secret}"),
    )
}

// `tree` with one usable bearer token seeded into it, and that token's
// plaintext.
pub(super) fn with_token(mut tree: serde_json::Value) -> (serde_json::Value, String) {
    let (entry, wire) = seeded_token(0);
    // APPENDED and not assigned: some fixtures ship their own `apiTokens` and
    // assert on entry 0 by id -- `secret_tree`'s `ci-deploy` entry, whose
    // digest is the redaction canary. Overwriting the array would take that
    // fixture away and the test would fail describing the wrong thing. The
    // seeded credential goes on the end, so entry 0 is whatever the caller put
    // there.
    match tree["access"]["apiTokens"].as_array_mut() {
        Some(existing) => existing.push(entry),
        None => tree["access"]["apiTokens"] = json!([entry]),
    }
    (tree, wire)
}

// A configured tree holding `count` usable tokens, with their plaintexts.
pub(super) fn token_tree(password: &str, count: usize) -> (serde_json::Value, Vec<String>) {
    let (entries, wires): (Vec<_>, Vec<_>) = (0..count).map(seeded_token).unzip();
    let mut tree = configured_tree(password);
    tree["access"]["apiTokens"] = json!(entries);
    (tree, wires)
}

// A request carrying a bearer token and **no cookie**, which is what makes
// every assertion below about the token rather than about the session.
pub(super) async fn bearer(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
) -> Response<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    send(router, builder.body(Body::empty()).unwrap()).await
}

// A form body carrying a bearer token and no cookie.
//
// The action routes take a form encoding rather than JSON, so the bearer
// equivalent of `post_form` is its own helper rather than a flag on one.
pub(super) async fn bearer_form(
    router: &Router,
    path: &str,
    token: &str,
    body: &str,
) -> Response<axum::body::Body> {
    let builder = Request::builder()
        .method("POST")
        .uri(path)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

// A JSON body carrying a bearer token and no cookie.
pub(super) async fn bearer_json(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: &str,
) -> Response<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {token}"));
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

// The pane's sentence, ratified and asserted
// byte for byte.

// The bootstrap end to end: a browser session mints, the plaintext appears
// once, the tree keeps only a digest, and the token then authenticates the
// API on its own.

// **Bearer only.** Every `/api/v1/` route takes the bearer, and a session
// cookie alone is a 401.

// The three token routes take a bearer and
// nothing else, and a session cookie presented to any of them is a 401.

// The three routes as a lifecycle: mint, list, revoke, and the revoked token
// stops being accepted on the next request.
#[tokio::test]
pub(super) async fn the_api_mints_lists_and_revokes() {
    let (tree, wires) = token_tree("hunter2secret", 1);
    let (router, _) = test_app(tree);

    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/tokens",
        &wires[0],
        r#"{"name":"ci-deploy"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_api_headers(&response, "POST /api/v1/tokens");
    let minted: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("a JSON body");
    let wire = minted["token"].as_str().expect("the plaintext").to_string();
    let id = minted["id"].as_str().expect("the id").to_string();
    assert_eq!(minted["name"], json!("ci-deploy"));
    assert!(crate::token::parse(&wire).is_some(), "{wire}");

    // The listing carries identity and never a secret — neither the digest
    // that is stored nor the plaintext that is not.
    let response = bearer(&router, "GET", "/api/v1/tokens", &wire).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        !body.contains(&wire),
        "the plaintext reached a listing: {body}"
    );
    assert!(
        !body.contains("hash"),
        "the digest reached a listing: {body}"
    );
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .expect("an array")
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["seeded-0", "ci-deploy"]);

    // Revocation takes effect on the next request.
    let response = bearer(&router, "DELETE", &format!("/api/v1/tokens/{id}"), &wire).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        bearer(&router, "GET", "/api/v1/tokens", &wire)
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "a revoked token must stop working"
    );
    assert_eq!(
        bearer(&router, "GET", "/api/v1/tokens", &wires[0])
            .await
            .status(),
        StatusCode::OK,
        "revoking one token must not revoke another"
    );
}

// The cap is answered at the route, in the caller's terms.

// A name the store would refuse is refused at the route, as a 422 about the
// body rather than as a failed write.
#[tokio::test]
pub(super) async fn a_name_the_store_refuses_is_a_422() {
    let (tree, wires) = token_tree("hunter2secret", 1);
    let (router, fake) = test_app(tree);

    for body in [r#"{"name":""}"#, r#"{"name":"ci\ndeploy"}"#] {
        let response = bearer_json(&router, "POST", "/api/v1/tokens", &wires[0], body).await;
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
    // And a body that is not this shape at all is a 400, not a 422.
    let response = bearer_json(&router, "POST", "/api/v1/tokens", &wires[0], "{}").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(envelope(response).await["code"], "request_invalid");

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// The collection error contract: a well-formed
// identifier that names nothing is **404**, and 422 is reserved for an
// identifier that is not well formed at all.
#[tokio::test]
pub(super) async fn an_absent_token_id_is_404_and_a_malformed_one_is_422() {
    let (tree, wires) = token_tree("hunter2secret", 1);
    let (tree, _token) = with_token(tree);
    let (router, fake) = test_app(tree);

    // Well formed, and no entry carries it.
    let response = bearer(&router, "DELETE", "/api/v1/tokens/deadbeef", &wires[0]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!("access.apiTokens"));

    // Not an identifier at all: well formed and absent is a different answer
    // from not well formed, and they must not share a status.
    for path in ["/api/v1/tokens/NOTHEX", "/api/v1/tokens/ci-deploy"] {
        let response = bearer(&router, "DELETE", path, &wires[0]).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{path}"
        );
    }

    // The empty spelling is not this route -- measured, and not assumed from
    // the rotate action, whose empty `{iface}` segment is interior rather than
    // trailing and IS served. `/api/v1/tokens/` reaches the reserved subtree's
    // own not-found handler. The gate releases the whole subtree either way,
    // so what this arm pins is which handler answers, in status, code and
    // body.
    let cookie = login(&router, "hunter2secret").await;
    let response = request(&router, "DELETE", "/api/v1/tokens/", Some(&cookie), None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(envelope(response).await["code"], "not_found");

    // And the bearer arm of the same path answers the same thing, because the
    // gate releases the whole reserved subtree and stops there: the credential
    // is never read, so it cannot decide the medium.
    // `an_undeclared_api_path_answers_the_404_envelope_whatever_the_credential`
    // is the general statement, and this arm holds it for this path.
    let response = bearer(&router, "DELETE", "/api/v1/tokens/", &wires[0]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(envelope(response).await["code"], "not_found");

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// Rule 1's medium rule, stated the way the gate has to implement it: a
// path inside the reserved `/api` subtree that no route declares answers
// the 404 envelope, and **which credential the request carried is not
// what decides that**. A client that addressed the JSON surface is answered
// in JSON whether it sent nothing at all, a token that is not stored, a
// token that is, or a session cookie.

// The same rule in **setup mode**, which is where the gate's *other* redirect
// lives and so is a second exit that has to be closed rather than the same
// one twice.

// **Bearer verification is not rate limited, and must not be.**

// The published document describes the token routes, and
// describes them as the code serves them.
#[test]
pub(super) fn the_openapi_document_covers_the_token_routes() {
    let document: serde_json::Value =
        serde_json::from_str(&crate::openapi::document_json().unwrap())
            .expect("the document is JSON");

    let collection = &document["paths"]["/api/v1/tokens"];
    for (method, statuses) in [
        ("get", vec!["200", "401"]),
        ("post", vec!["201", "400", "401", "409", "422"]),
    ] {
        for status in statuses {
            assert!(
                collection[method]["responses"][status].is_object(),
                "{method} /api/v1/tokens must document {status}: {collection}"
            );
        }
    }

    let item = &document["paths"]["/api/v1/tokens/{id}"]["delete"]["responses"];
    for status in ["204", "401", "404", "422"] {
        assert!(
            item[status].is_object(),
            "DELETE must document {status}: {item}"
        );
    }

    // The listing's row carries identity and never a secret, in the document
    // as well as on the wire.
    let summary = &document["components"]["schemas"]["ApiTokenSummary"]["properties"];
    let members: Vec<&str> = summary
        .as_object()
        .expect("properties")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(members, vec!["created", "id", "name"], "{summary}");

    // The plaintext is a member of the mint's response and of nothing else.
    let minted = &document["components"]["schemas"]["MintedToken"]["properties"];
    assert!(minted["token"].is_object(), "{minted}");
}
