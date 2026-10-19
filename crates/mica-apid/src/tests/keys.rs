//! SSH authorized keys.

use crate::auth;
use crate::settings_api::{FakeSettings, SettingsApi};
use axum::http::StatusCode;
use serde_json::json;

use super::*;

// Real `ssh-keygen` output, the same three keys `micad/micad/src/reconciler/
// sshd.rs` tests against, so both sides of the D-Bus boundary are exercised
// with identical input. Public keys are not secrets; these correspond to no
// device and the private halves were discarded at generation.
pub(super) const REAL_ED25519_LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIL99V7xPTOP3jZjnbVPM7xC+ckwzkOQPalUpsvtPzYo8 rfct-034-test-ed25519";
pub(super) const REAL_RSA_LINE: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDT2F3imgGgI+xGNSQI+0alU1qRwyU3gCc8wU6msXSzZsVc8OYlg4VIqxsV/GLpBmgRz5lGoxjTT2TU0t1VwaMs845NqRIWzpG88ohD1LMn7RnUrNTxf4syFuvmELmYstqMfc6Q6rApqFoA6023Rl2orgd8N3SQ2wPAw8Rk9OLwim9/R7tX8C8FTbnMtepzTvOUNGTDAaKYhTZZnZpsGCwKa9f2aWyaS2XqLwn9uWpmHRUAkV10l45W2rLhnceejwwHotlZUIAFt8rlmS1ojRaLWqECVAuO5CDTt64KLLRniw8yHIYsWkeVsHZXCxq+J7oUVI3ogOSYs1M4I2eFCccD rfct-034-test-rsa";

// Fingerprints as `ssh-keygen -lf` printed them for the three keys above.
// Comparing apid's fingerprint against values that came out of OpenSSH is the
// point: a fingerprint checked only against itself proves nothing, and this
// one is the handle a removal is addressed by.
pub(super) const REAL_ED25519_FINGERPRINT: &str =
    "SHA256:HrgN3GLi6Mop2uSRjgOoxImM8zRkFmgqCKoeGD9QOaM";
pub(super) const REAL_RSA_FINGERPRINT: &str = "SHA256:zv0xTYuVTo5pFpcl/svzzz/vJFvoguWxKlghlXQS1bE";
pub(super) const REAL_ED25519_SECOND_FINGERPRINT: &str =
    "SHA256:d7yiR/zCsNFh8WmU6CGLWEG5vE06icIelqVoNc8TT2E";

// Percent-encode one form value.
//
// Everything outside the unreserved set is escaped, including the space, so a
// case that is about a stray space or a control character survives the trip
// to the handler as the byte it is meant to be.
pub(super) fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// The canonical `<type> <blob>` half of a full `ssh-keygen` line.
pub(super) fn canonical(line: &str) -> String {
    let mut fields = line.splitn(3, ' ');
    let key_type = fields.next().unwrap();
    let blob = fields.next().unwrap();
    format!("{key_type} {blob}")
}

// The comment half of a full `ssh-keygen` line.
pub(super) fn comment_of(line: &str) -> &str {
    line.splitn(3, ' ').nth(2).unwrap()
}

// A configured tree carrying an `access.ssh` subtree holding `keys`.
pub(super) fn ssh_tree(keys: serde_json::Value) -> serde_json::Value {
    let hash = auth::hash_password("hunter2secret").unwrap();
    json!({
        "hostname": "mica",
        "network": {},
        "access": {
            "webAdmin": { "password_hash": hash },
            "ssh": {
                "enabled": false,
                "port": 22,
                "permitRootLogin": true,
                "passwordAuthentication": true,
                "listenAddresses": [],
                "authorizedKeys": keys,
            },
        },
    })
}

// The stored form of one parsed key: comment split out of the key text.
pub(super) fn stored_key(line: &str) -> serde_json::Value {
    json!({ "key": canonical(line), "comment": comment_of(line) })
}

// One of an unbounded family of distinct, structurally real ed25519 key lines.
pub(super) fn generated_key_line(index: u8) -> String {
    let blob = REAL_ED25519_LINE
        .split(' ')
        .nth(1)
        .expect("the fixture is `<type> <blob> <comment>`");
    let mut bytes = micad_settings::decode_base64(blob).expect("the fixture blob decodes");
    let last = bytes.len() - 1;
    bytes[last] = index;
    format!(
        "ssh-ed25519 {}",
        micad_settings::encode_base64_nopad(&bytes)
    )
}

// The stored key list, as JSON.
pub(super) async fn stored_key_list(fake: &FakeSettings) -> serde_json::Value {
    fake.get_settings("access.ssh.authorizedKeys")
        .await
        .unwrap()
}

// The sentence required on the listing and
// on the add, spelled out here rather than read from the constant: a test that
// compares the code against itself cannot notice the sentence being reworded.
pub(super) const ROOT_KEY_NOTICE_TEXT: &str = "Every authorized key is a root key.";

// The SSH collection's dot-path, as every envelope it raises names it.
pub(super) const SSH_KEYS_DOT_PATH: &str = "access.ssh.authorizedKeys";

// The WiFi collection's dot-path.
pub(super) const WIFI_NETWORKS_DOT_PATH: &str = "wifi.client.networks";

// The item route of one stored key.
pub(super) fn ssh_key_url(fingerprint: &str) -> String {
    format!(
        "/api/v1/ssh/authorized-keys/{}",
        fingerprint.replace('/', "%2F")
    )
}

// The collection end to end: an empty listing, an add, a listing that shows
// it, and a removal addressed by the fingerprint the add returned.
#[tokio::test]
pub(super) async fn the_ssh_key_collection_lists_adds_and_removes() {
    let (tree, token) = with_token(ssh_tree(json!([])));
    let (router, fake) = test_app(tree);

    let empty = bearer(&router, "GET", "/api/v1/ssh/authorized-keys", &token).await;
    assert_eq!(empty.status(), StatusCode::OK);
    let empty = body_json(empty).await;
    assert_eq!(empty["keys"], json!([]));

    let added = bearer_json(
        &router,
        "POST",
        "/api/v1/ssh/authorized-keys",
        &token,
        &json!({ "key": REAL_ED25519_LINE }).to_string(),
    )
    .await;
    assert_eq!(added.status(), StatusCode::CREATED);
    let added = body_json(added).await;
    // Canonicalised by the parser: the comment is lifted out of `key` so the
    // same key pasted under two labels is one key.
    assert_eq!(added["key"]["key"], json!(canonical(REAL_ED25519_LINE)));
    assert_eq!(
        added["key"]["comment"],
        json!(comment_of(REAL_ED25519_LINE))
    );
    assert_eq!(added["key"]["fingerprint"], json!(REAL_ED25519_FINGERPRINT));
    assert_eq!(
        stored_key_list(&fake).await,
        json!([stored_key(REAL_ED25519_LINE)])
    );
    assert_eq!(fake.set_paths(), vec![SSH_KEYS_DOT_PATH]);

    let listed =
        body_json(bearer(&router, "GET", "/api/v1/ssh/authorized-keys", &token).await).await;
    assert_eq!(listed["keys"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["keys"][0]["fingerprint"],
        json!(REAL_ED25519_FINGERPRINT)
    );

    let removed = bearer(
        &router,
        "DELETE",
        &ssh_key_url(REAL_ED25519_FINGERPRINT),
        &token,
    )
    .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(stored_key_list(&fake).await, json!([]));
}

// The notice is on **both** answers, on purpose: a
// client that only ever adds keys is still told that a key added here logs in
// as root.
#[tokio::test]
pub(super) async fn the_root_key_notice_is_on_the_listing_and_on_the_add() {
    let (tree, token) = with_token(ssh_tree(json!([])));
    let (router, _) = test_app(tree);

    let listed =
        body_json(bearer(&router, "GET", "/api/v1/ssh/authorized-keys", &token).await).await;
    assert_eq!(listed["notice"], json!(ROOT_KEY_NOTICE_TEXT));

    let added = bearer_json(
        &router,
        "POST",
        "/api/v1/ssh/authorized-keys",
        &token,
        &json!({ "key": REAL_ED25519_LINE }).to_string(),
    )
    .await;
    assert_eq!(
        body_json(added).await["notice"],
        json!(ROOT_KEY_NOTICE_TEXT)
    );
}

// The add runs the parser the pane runs, and refuses the same lines: both
// surfaces reach `parse_authorized_key`, so a line one accepts is a line the
// other accepts and a line micad would reject reaches neither.

// A key already stored is refused at **409 `key_exists`**.
#[tokio::test]
pub(super) async fn a_duplicate_key_is_409_and_the_stored_list_is_unchanged() {
    let (tree, token) = with_token(ssh_tree(json!([stored_key(REAL_ED25519_LINE)])));
    let (router, fake) = test_app(tree);

    let relabelled = format!("{} someone-else", canonical(REAL_ED25519_LINE));
    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/ssh/authorized-keys",
        &token,
        &json!({ "key": relabelled }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "key_exists");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!(SSH_KEYS_DOT_PATH));
    // The message must not be the validator's -- this route decided the answer
    // and did not recover it from a sentence.
    assert!(
        !error["message"].as_str().unwrap().contains("entry 0"),
        "the refusal echoed the validator's wording: {error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(
        stored_key_list(&fake).await,
        json!([stored_key(REAL_ED25519_LINE)])
    );

    // And a malformed key is still 422: the two conditions answer two
    // different statuses.
    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/ssh/authorized-keys",
        &token,
        &json!({ "key": "ssh-ed25519 not-base64" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "validation_failed");
}

// The 32-key cap is **409 `key_limit_reached`**, answered from the exported
// bound exactly as the token mint answers its own from `MAX_TOKENS`.
#[tokio::test]
pub(super) async fn a_full_key_list_is_409_and_names_the_bound() {
    let full: Vec<serde_json::Value> = (0..micad_settings::MAX_KEYS)
        .map(|index| json!({ "key": generated_key_line(index as u8) }))
        .collect();
    let (tree, token) = with_token(ssh_tree(json!(full)));
    let (router, fake) = test_app(tree);

    // A key no stored entry carries, so the duplicate rule above cannot be what
    // answers: the two 409s must be told apart by their code.
    let response = bearer_json(
        &router,
        "POST",
        "/api/v1/ssh/authorized-keys",
        &token,
        &json!({ "key": generated_key_line(micad_settings::MAX_KEYS as u8) }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "key_limit_reached");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(&micad_settings::MAX_KEYS.to_string()),
        "the refusal must name the bound: {error}"
    );
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// Section 2.4's rule on the SSH item route: a well-formed fingerprint that
// matches no key is **404**, and a string that is not a fingerprint at all is
// **422**.
#[tokio::test]
pub(super) async fn an_absent_key_fingerprint_is_404_where_the_pane_is_422() {
    let (tree, token) = with_token(ssh_tree(json!([stored_key(REAL_ED25519_LINE)])));
    let (router, fake) = test_app(tree);

    // Well formed -- it is a real fingerprint of a real key -- and no stored
    // entry carries it.
    let response = bearer(
        &router,
        "DELETE",
        &ssh_key_url(REAL_ED25519_SECOND_FINGERPRINT),
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["source"], "apid");
    assert_eq!(error["path"], json!(SSH_KEYS_DOT_PATH));

    // Not an identifier at all. The last of these is the canonical key text,
    // which the pane accepts as an identifier and this route does not: on a
    // path segment there is one interpretation, and it is the fingerprint.
    for identifier in [
        "SHA256:tooshort",
        "HrgN3GLi6Mop2uSRjgOoxImM8zRkFmgqCKoeGD9QOaM",
        "SHA1:HrgN3GLi6Mop2uSRjgOoxImM8zRkFmgqCKoeGD9QOa",
        &canonical(REAL_ED25519_LINE).replace(' ', "%20"),
    ] {
        let response = bearer(&router, "DELETE", &ssh_key_url(identifier), &token).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{identifier}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{identifier}"
        );
    }

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(
        stored_key_list(&fake).await,
        json!([stored_key(REAL_ED25519_LINE)])
    );
}

// The HTML half of the split recorded here:
// the pane answers **422** where `DELETE /api/v1/ssh/authorized-keys/
// {fingerprint}` answers **404**, on the same condition.

// A fingerprint's base64 alphabet contains `/`, so the identifier of a real
// RSA key is two path segments unless it is percent-encoded. Measured rather
// than assumed: `%2F` is three characters at match time, so the route matches
// one segment, and axum decodes it back to a `/` before the handler sees it.
#[tokio::test]
pub(super) async fn a_fingerprint_carrying_a_slash_is_addressable_percent_encoded() {
    assert!(
        REAL_RSA_FINGERPRINT.contains('/'),
        "this test is about the `/`, and the fixture no longer has one"
    );
    let (tree, token) = with_token(ssh_tree(json!([
        stored_key(REAL_RSA_LINE),
        stored_key(REAL_ED25519_LINE),
    ])));
    let (router, fake) = test_app(tree);

    let response = bearer(
        &router,
        "DELETE",
        &ssh_key_url(REAL_RSA_FINGERPRINT),
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        stored_key_list(&fake).await,
        json!([stored_key(REAL_ED25519_LINE)]),
        "the RSA key and only the RSA key was removed"
    );

    // Unencoded, the same fingerprint is two segments and names no route at
    // all -- which is the reserved subtree's own not-found answer and not this
    // collection's 404.
    let raw = format!("/api/v1/ssh/authorized-keys/{REAL_RSA_FINGERPRINT}");
    // The COOKIE and not the bearer, deliberately: this path is UNDECLARED, so
    // it reaches the gate rather than a route's own extractor, and the gate
    // takes the session and only the session.
    let cookie = login(&router, "hunter2secret").await;
    let response = request(&router, "DELETE", &raw, Some(&cookie), None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(envelope(response).await["code"], "not_found");
}
