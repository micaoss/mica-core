//! The owner certificates the kernel trusts driver extensions by, and the
//! owner's driver extensions. micad changes them, each change asking for the
//! administrator password, as a task of kind `drivers.apply`; every change
//! applies from the next boot.

use std::future::Future;
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::audit::Source;

use super::*;

/// The largest upload of a build, its files together.
pub(super) const MAX_EXTENSION_UPLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// The largest certificate file, PEM or DER.
const MAX_CERTIFICATE_BYTES: usize = 64 * 1024;

const OWNER_CERT_ADD_EVENT: &str = "owner-cert-add";
const OWNER_CERT_REMOVE_EVENT: &str = "owner-cert-remove";
const EXTENSION_INSTALL_EVENT: &str = "driver-extension-install";
const EXTENSION_REMOVE_EVENT: &str = "driver-extension-remove";
const EXTENSION_DROP_EVENT: &str = "driver-extension-drop";

/// `POST /api/v1/system/owner-certs` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerCertAdd {
    /// The administrator password, checked whatever the session or token.
    pub(super) password: String,
    /// The certificate file, PEM or DER, in base64.
    pub(super) certificate: String,
}

/// The request body of a removal: the administrator password, checked
/// whatever the session or token.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DriverPassword {
    pub(super) password: String,
}

/// `POST /api/v1/system/extensions/{name}/drop` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionDrop {
    /// The administrator password, checked whatever the session or token.
    pub(super) password: String,
    /// The kernel of the system update that waits for the extension, as
    /// `waitsForExtension` names it.
    pub(super) kernel: String,
}

/// The fields of `POST /api/v1/system/extensions`: `password`, and either
/// `archive` or `descriptor`, `image` and `signature`.
#[derive(utoipa::ToSchema)]
#[allow(dead_code)]
pub(crate) struct ExtensionUpload {
    /// The administrator password, checked whatever the session or token.
    password: String,
    /// The build's `MICAUPD1` archive.
    #[schema(value_type = Option<String>, format = Binary)]
    archive: Option<Vec<u8>>,
    /// The build's `ext.json`.
    #[schema(value_type = Option<String>, format = Binary)]
    descriptor: Option<Vec<u8>>,
    /// The build's image, `ext.img`.
    #[schema(value_type = Option<String>, format = Binary)]
    image: Option<Vec<u8>>,
    /// The root hash signature, `ext.roothash.p7s`.
    #[schema(value_type = Option<String>, format = Binary)]
    signature: Option<Vec<u8>>,
}

fn refuse(status: StatusCode, code: &'static str, message: &str) -> Response {
    api_response(status, ApiError::apid(code, message.to_string()))
}

fn password_required() -> Response {
    api_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::apid(
            "validation_failed",
            "the administrator password is required".to_string(),
        )
        .at("password"),
    )
}

/// micad's `DriversRefused`, as the code, the status it answers and the
/// message.
fn drivers_refusal(err: &anyhow::Error) -> Option<(&'static str, StatusCode, String)> {
    let (name, description) = err.downcast_ref::<zbus::Error>().and_then(error_reply)?;
    if name != MICAD_DRIVERS_REFUSED {
        return None;
    }
    let refusal: Value = serde_json::from_str(&description?).ok()?;
    let (code, status) = match refusal["code"].as_str()? {
        "password_incorrect" => ("password_incorrect", StatusCode::FORBIDDEN),
        "login_throttled" => ("login_throttled", StatusCode::TOO_MANY_REQUESTS),
        "not_claimed" => ("not_claimed", StatusCode::CONFLICT),
        _ => return None,
    };
    Some((code, status, refusal["message"].as_str()?.to_string()))
}

/// Run a change that asks for the administrator `password` as the factory
/// reset does: counted in the login backoff, recorded under `event`, and
/// answered with the task micad queued.
async fn confirmed<F, W>(
    state: &AppState,
    credential: &ApiCredential,
    source: &str,
    event: &str,
    password: &str,
    change: F,
) -> Response
where
    F: FnOnce(&'static str) -> W,
    W: Future<Output = anyhow::Result<String>>,
{
    if password.is_empty() {
        return password_required();
    }
    if !state.guard.begin_attempt() {
        state.audit.record(event, "throttled", source);
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "login_throttled",
            "too many failed attempts; retry shortly",
        );
    }
    let by = match credential {
        ApiCredential::Session(_) => "console",
        ApiCredential::Bearer | ApiCredential::Local => "api",
    };
    let err = match change(by).await {
        Ok(task_id) => {
            state.guard.record_success();
            state.audit.record(event, "requested", source);
            return api_response(StatusCode::ACCEPTED, TaskAccepted { task_id });
        }
        Err(err) => err,
    };
    let Some((code, status, message)) = drivers_refusal(&err) else {
        return bus_api_error(&err, None);
    };
    let outcome = match code {
        "password_incorrect" => {
            state.guard.confirm_failure();
            "wrong-password"
        }
        "login_throttled" => "throttled",
        _ => "refused",
    };
    state.audit.record(event, outcome, source);
    let error = ApiError::micad(code, message);
    api_response(
        status,
        if code == "password_incorrect" {
            error.at("password")
        } else {
            error
        },
    )
}

/// micad's answer of a read, passed through.
fn read(answer: anyhow::Result<Value>) -> Response {
    match answer {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, None),
    }
}

/// The enrolled owner certificates, each with its `sha256`, `state`
/// (`loaded`, `rejected` with its `reason`, `pending-next-boot`,
/// `pending-confirmation` or `removed-next-boot`) and, while it reads,
/// `subject`, `issuer`, `notBefore`, `notAfter` and `key`.
#[utoipa::path(
    get,
    path = V1_OWNER_CERTS_PATH,
    context_path = API,
    tag = "observation",
    responses(
        (status = 200, description = "The enrolled certificates", body = Value),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "mica-deploy failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_owner_certs(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    read(state.api.get_owner_certs().await)
}

/// Enrol an owner certificate from the next boot: one X.509 v3
/// certificate, PEM or DER, with an RSA key of 2048 bits or more or an ECDSA
/// key on P-256 or P-384; its validity period is held to the clock while the
/// device believes it. **The administrator password is required**, whatever
/// the session or token, and a wrong one counts in the login backoff. The
/// task's message is the refusal, or the warning of a certificate whose
/// validity period was not checked.
#[utoipa::path(
    post,
    path = V1_OWNER_CERTS_PATH,
    context_path = API,
    tag = "actions",
    request_body = OwnerCertAdd,
    responses(
        (status = 202, description = "The enrolment was queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device is not claimed (`not_claimed`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the password is empty, or the certificate is not base64 of at most 64 KiB (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_owner_cert_add(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: OwnerCertAdd = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let Some(certificate) = micad_settings::decode_base64(request.certificate.trim())
        .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_CERTIFICATE_BYTES)
    else {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the certificate is the base64 of a PEM or DER file of at most 64 KiB".to_string(),
            )
            .at("certificate"),
        );
    };
    let (api, secret, from) = (
        Arc::clone(&state.api),
        request.password.clone(),
        source.clone(),
    );
    confirmed(
        &state,
        &credential,
        &source,
        OWNER_CERT_ADD_EVENT,
        &request.password,
        |by| async move { api.add_owner_cert(&certificate, &secret, by, &from).await },
    )
    .await
}

/// Remove an enrolled owner certificate from the next boot; this boot's
/// keyring keeps it. The password is asked as for an enrolment.
#[utoipa::path(
    delete,
    path = V1_OWNER_CERT_ROUTE,
    context_path = API,
    tag = "actions",
    params(("sha256" = String, Path, description = "The certificate's SHA-256")),
    request_body = DriverPassword,
    responses(
        (status = 202, description = "The removal was queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device is not claimed (`not_claimed`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the password is empty, or the path names no SHA-256 (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_owner_cert_remove(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Path(sha256): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: DriverPassword = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let (api, secret, from) = (
        Arc::clone(&state.api),
        request.password.clone(),
        source.clone(),
    );
    confirmed(
        &state,
        &credential,
        &source,
        OWNER_CERT_REMOVE_EVENT,
        &request.password,
        |by| async move { api.remove_owner_cert(&sha256, &secret, by, &from).await },
    )
    .await
}

/// The driver extensions: `installed` (the confirmed builds, by kernel and
/// name), `pending` (a change on trial and its `attemptsLeft`), `pair` (one
/// paired with a system update), `pendingVerification` (builds the kernel
/// checks at the next boot that maps them), `waitsForExtension` (what the
/// selected system update waits for, `[{name, kernel}]`), `boot` (what this
/// boot mapped and refused), `trust` and the running `kernel`.
#[utoipa::path(
    get,
    path = V1_EXTENSIONS_PATH,
    context_path = API,
    tag = "observation",
    responses(
        (status = 200, description = "The driver extensions", body = Value),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "mica-deploy failed (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_extensions(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    read(state.api.get_extensions().await)
}

/// The files of an upload, removed when dropped unless kept.
struct Uploaded(Vec<PathBuf>);

impl Drop for Uploaded {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Install an owner's driver extension build from the next boot, on trial:
/// `multipart/form-data` with `password` and either `archive` (the build's
/// `MICAUPD1` archive) or `descriptor`, `image` and `signature` (its
/// `ext.json`, `ext.img` and `ext.roothash.p7s`). A build for the kernel of
/// the system update a check selected stays staged for that update. The
/// password is asked as for an owner certificate.
#[utoipa::path(
    post,
    path = V1_EXTENSIONS_PATH,
    context_path = API,
    tag = "actions",
    request_body(content = ExtensionUpload, content_type = "multipart/form-data"),
    responses(
        (status = 202, description = "The build was taken and its install queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The upload was interrupted or is not multipart (`extension_upload_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device is not claimed (`not_claimed`)", body = ApiError),
        (status = 413, description = "The files are above 256 MiB together (`extension_too_large`)", body = ApiError),
        (status = 422, description = "The password is empty, or the fields are not `archive` or `descriptor`, `image` and `signature` (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 507, description = "The upload could not be written (`extension_storage_unavailable`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_extension_install(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Response {
    use tokio::io::AsyncWriteExt as _;

    let invalid = || {
        refuse(
            StatusCode::BAD_REQUEST,
            "extension_upload_invalid",
            "the upload is not complete multipart/form-data",
        )
    };
    let storage = || {
        refuse(
            StatusCode::INSUFFICIENT_STORAGE,
            "extension_storage_unavailable",
            "the upload could not be written",
        )
    };
    let fields = || {
        api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "upload password and either archive, or descriptor, image and signature"
                    .to_string(),
            ),
        )
    };
    let Ok(mut multipart) = multipart else {
        return invalid();
    };
    let uploads = state.update_uploads.as_path();
    if tokio::fs::create_dir_all(uploads).await.is_err() {
        return storage();
    }
    let mut uploaded = Uploaded(Vec::new());
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut password = String::new();
    let mut written = 0_u64;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return invalid(),
        };
        let name = field.name().unwrap_or_default().to_string();
        if name == "password" {
            match field.text().await {
                Ok(text) if text.len() <= 1024 => password = text,
                _ => return invalid(),
            }
            continue;
        }
        if !["archive", "descriptor", "image", "signature"].contains(&name.as_str())
            || files.iter().any(|(held, _)| *held == name)
        {
            return fields();
        }
        // A name drawn here, never the client's: micad takes the file by the
        // path apid names.
        let path = uploads.join(format!(
            "{:032x}.{name}",
            u128::from_ne_bytes(micad_settings::random_bytes())
        ));
        let Ok(mut file) = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        else {
            return storage();
        };
        uploaded.0.push(path.clone());
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => return invalid(),
            };
            written += chunk.len() as u64;
            if written > MAX_EXTENSION_UPLOAD_BYTES {
                return refuse(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "extension_too_large",
                    "this device accepts a driver extension build of at most 256 MiB",
                );
            }
            if file.write_all(&chunk).await.is_err() {
                return storage();
            }
        }
        if file.sync_all().await.is_err() {
            return storage();
        }
        files.push((name, path));
    }
    files.sort_by_key(|(name, _)| match name.as_str() {
        "archive" | "descriptor" => 0,
        "image" => 1,
        _ => 2,
    });
    let paths: Vec<String> = files
        .iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect();
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    if names != ["archive"] && names != ["descriptor", "image", "signature"] {
        return fields();
    }
    let (api, secret, from) = (Arc::clone(&state.api), password.clone(), source.clone());
    let response = confirmed(
        &state,
        &credential,
        &source,
        EXTENSION_INSTALL_EVENT,
        &password,
        |by| async move {
            match paths.as_slice() {
                [archive] => api.install_extension(archive, &secret, by, &from).await,
                [descriptor, image, signature] => {
                    api.install_extension_files([descriptor, image, signature], &secret, by, &from)
                        .await
                }
                _ => unreachable!("the fields were checked"),
            }
        },
    )
    .await;
    // What micad took is micad's to remove; whatever it did not, apid's.
    if response.status() == StatusCode::ACCEPTED {
        uploaded.0.clear();
    }
    response
}

/// Stop installing a driver extension, on trial from the next boot. The
/// password is asked as for an owner certificate.
#[utoipa::path(
    delete,
    path = V1_EXTENSION_ROUTE,
    context_path = API,
    tag = "actions",
    params(("name" = String, Path, description = "The extension")),
    request_body = DriverPassword,
    responses(
        (status = 202, description = "The removal was queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device is not claimed (`not_claimed`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the password is empty, or the path names no extension (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_extension_remove(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Path(name): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: DriverPassword = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let (api, secret, from) = (
        Arc::clone(&state.api),
        request.password.clone(),
        source.clone(),
    );
    confirmed(
        &state,
        &credential,
        &source,
        EXTENSION_REMOVE_EVENT,
        &request.password,
        |by| async move { api.remove_extension(&name, &secret, by, &from).await },
    )
    .await
}

/// Let the system update to a new kernel run without a driver extension it
/// waits for (`waitsForExtension`): its install leaves the extension out. The
/// password is asked as for an owner certificate.
#[utoipa::path(
    post,
    path = V1_EXTENSION_DROP_ROUTE,
    context_path = API,
    tag = "actions",
    params(("name" = String, Path, description = "The extension")),
    request_body = ExtensionDrop,
    responses(
        (status = 202, description = "The decision was queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "The password is not the administrator's (`password_incorrect`), or a browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "The device is not claimed (`not_claimed`)", body = ApiError),
        (status = 422, description = "The body is not this shape, the password is empty, or the path or the kernel names nothing (`validation_failed`)", body = ApiError),
        (status = 429, description = "Too many wrong passwords; retry shortly (`login_throttled`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_extension_drop(
    credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Path(name): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: ExtensionDrop = match json_body(body, None) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let (api, secret, from) = (
        Arc::clone(&state.api),
        request.password.clone(),
        source.clone(),
    );
    confirmed(
        &state,
        &credential,
        &source,
        EXTENSION_DROP_EVENT,
        &request.password,
        |by| async move {
            api.drop_extension(&name, &request.kernel, &secret, by, &from)
                .await
        },
    )
    .await
}
