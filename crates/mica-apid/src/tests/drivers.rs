//! The owner certificates and driver extensions: the administrator password
//! on every change, the uploads a build arrives in, and the trail.

use super::*;

const PASSWORD: &str = "hunter2secret";
const CERTS: &str = "/api/v1/system/owner-certs";
const EXTENSIONS: &str = "/api/v1/system/extensions";
const BOUNDARY: &str = "mica-test-boundary";

fn device() -> (Router, Arc<FakeSettings>, TempDir, TempDir, String) {
    let (dir, uploads) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (tree, token) = with_token(configured_tree(PASSWORD));
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY)
        .with_persistence(dir.path())
        .with_update_uploads(uploads.path()));
    (router, fake, dir, uploads, token)
}

/// A `multipart/form-data` body of `fields`.
fn multipart(fields: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend(*value);
        body.extend(b"\r\n");
    }
    body.extend(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn upload(router: &Router, token: &str, fields: &[(&str, &[u8])]) -> Response<Body> {
    bearer_bytes(
        router,
        EXTENSIONS,
        multipart(fields),
        &format!("multipart/form-data; boundary={BOUNDARY}"),
        token,
    )
    .await
}

fn left(uploads: &TempDir) -> usize {
    std::fs::read_dir(uploads.path()).unwrap().count()
}

#[tokio::test]
pub(super) async fn a_certificate_is_enrolled_with_the_password_and_recorded() {
    let (router, fake, dir, _uploads, token) = device();
    let certificate = micad_settings::encode_base64_nopad(b"-----BEGIN CERTIFICATE-----");
    let response = bearer_json(
        &router,
        "POST",
        CERTS,
        &token,
        &json!({ "password": PASSWORD, "certificate": certificate }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    let changes = fake.driver_changes();
    assert_eq!(changes.len(), 1);
    assert!(
        changes[0].starts_with("add -----BEGIN CERTIFICATE----- api "),
        "{changes:?}"
    );
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("owner-cert-add".to_string(), "requested".to_string())]
    );

    let sha = "a".repeat(64);
    let response = bearer_json(
        &router,
        "DELETE",
        &format!("{CERTS}/{sha}"),
        &token,
        &json!({ "password": PASSWORD }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(fake.driver_changes()[1].starts_with(&format!("remove {sha} api ")));
}

#[tokio::test]
pub(super) async fn a_wrong_or_missing_password_or_certificate_changes_nothing() {
    let (router, fake, dir, _uploads, token) = device();
    let certificate = micad_settings::encode_base64_nopad(b"PEM");
    for (body, status, code, at) in [
        (
            json!({ "password": "guess", "certificate": certificate }),
            StatusCode::FORBIDDEN,
            "password_incorrect",
            "password",
        ),
        (
            json!({ "password": "", "certificate": certificate }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "password",
        ),
        (
            json!({ "password": PASSWORD, "certificate": "not base64!" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "certificate",
        ),
    ] {
        let response = bearer_json(&router, "POST", CERTS, &token, &body.to_string()).await;
        assert_eq!(response.status(), status, "{body}");
        let envelope = envelope(response).await;
        assert_eq!(envelope["code"], code, "{body}");
        assert_eq!(envelope["path"], at, "{body}");
    }
    assert!(fake.driver_changes().is_empty());
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("owner-cert-add".to_string(), "wrong-password".to_string())]
    );
}

#[tokio::test]
pub(super) async fn a_build_of_three_files_is_handed_to_micad_which_keeps_them() {
    let (router, fake, _dir, uploads, token) = device();
    let response = upload(
        &router,
        &token,
        &[
            ("signature", b"S"),
            ("password", PASSWORD.as_bytes()),
            ("descriptor", b"D"),
            ("image", b"I"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let changes = fake.driver_changes();
    assert!(
        changes[0].starts_with("install-files D,I,S api "),
        "{changes:?}"
    );
    assert_eq!(left(&uploads), 3, "micad's to remove once read");

    let response = upload(
        &router,
        &token,
        &[("archive", b"MICAUPD1"), ("password", PASSWORD.as_bytes())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(fake.driver_changes()[1].starts_with("install MICAUPD1 api "));
}

#[tokio::test]
pub(super) async fn a_refused_upload_leaves_nothing() {
    let (router, fake, _dir, uploads, token) = device();
    for (fields, status) in [
        (
            vec![
                ("password", PASSWORD.as_bytes()),
                ("descriptor", b"D".as_slice()),
            ],
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            vec![
                ("password", PASSWORD.as_bytes()),
                ("archive", b"A".as_slice()),
                ("image", b"I".as_slice()),
            ],
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            vec![
                ("password", b"guess".as_slice()),
                ("archive", b"A".as_slice()),
            ],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![("archive", b"A".as_slice())],
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let response = upload(&router, &token, &fields).await;
        assert_eq!(response.status(), status);
        assert_eq!(left(&uploads), 0);
    }
    let response = bearer_bytes(&router, EXTENSIONS, b"x".to_vec(), "text/plain", &token).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(envelope(response).await["code"], "extension_upload_invalid");
    assert!(fake.driver_changes().is_empty());
}

#[tokio::test]
pub(super) async fn an_extension_is_removed_or_dropped_with_the_password() {
    let (router, fake, _dir, _uploads, token) = device();
    let response = bearer_json(
        &router,
        "DELETE",
        &format!("{EXTENSIONS}/rtl8125"),
        &token,
        &json!({ "password": PASSWORD }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let kernel = "b".repeat(64);
    let response = bearer_json(
        &router,
        "POST",
        &format!("{EXTENSIONS}/rtl8125/drop"),
        &token,
        &json!({ "password": PASSWORD, "kernel": kernel }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let changes = fake.driver_changes();
    assert!(changes[0].starts_with("remove-extension rtl8125 api "));
    assert!(changes[1].starts_with(&format!("drop rtl8125 {kernel} api ")));
}

#[tokio::test]
pub(super) async fn the_reads_are_micad_s() {
    let (router, _fake, _dir, _uploads, token) = device();
    let response = bearer_json(&router, "GET", CERTS, &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!([]));
    let response = bearer_json(&router, "GET", EXTENSIONS, &token, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["kernel"], "k");
}
