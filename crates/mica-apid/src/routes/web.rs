//! Where the console listens.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;
use crate::audit::Source;
use crate::tls::CertificateInfo;

/// The `access.web` subtree as the device holds it.
///
/// Every field is required on a write: a `PUT` replaces the subtree.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WebConfiguration {
    /// TCP port of the HTTP listener. It serves the console, or only
    /// redirects to HTTPS when that is enabled. Default 8080.
    pub(super) http_port: u16,
    /// Whether apid serves HTTPS with its self-signed identity. Default off.
    pub(super) https_enabled: bool,
    /// TCP port of the HTTPS listener. Default 8443.
    pub(super) https_port: u16,
}

/// Read where the console listens.
#[utoipa::path(
    get,
    path = V1_WEB_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The stored `access.web` subtree", body = WebConfiguration),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The stored subtree is not one this build can read (`settings_invalid`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_web_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let stored = match state.api.get_settings(WEB_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(WEB_SETTINGS_PATH)),
    };
    match serde_json::from_value::<WebConfiguration>(stored) {
        Ok(configured) => api_response(StatusCode::OK, configured),
        Err(err) => api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored web settings could not be read: {err}"),
            )
            .at(WEB_SETTINGS_PATH),
        ),
    }
}

/// Move the console: its ports, and whether it serves HTTPS.
///
/// **A `PUT` replaces the whole subtree.** Answers the apply task's id; the
/// `web` reconciler then restarts apid on the new listeners, so this
/// connection ends and the console is reached at the new address. With HTTPS
/// enabled the HTTP port only redirects to it.
#[utoipa::path(
    put,
    path = V1_WEB_PATH,
    context_path = API,
    tag = "resources",
    request_body = WebConfiguration,
    responses(
        (status = 202, description = "The configuration was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not a web document or a port is 0 (`validation_failed`); or micad rejected it, for a port another listener holds (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_web_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let configured: WebConfiguration = match json_body(body, Some(WEB_SETTINGS_PATH)) {
        Ok(configured) => configured,
        Err(response) => return *response,
    };
    if configured.http_port == 0 || configured.https_port == 0 {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "a port is a TCP port between 1 and 65535; the defaults are 8080 and 8443"
                    .to_string(),
            )
            .at(WEB_SETTINGS_PATH),
        );
    }
    // Infallible: a struct of scalars.
    let value = match encode(&configured) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(WEB_SETTINGS_PATH, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(WEB_SETTINGS_PATH)),
    }
}

/// The console's TLS identity, as `GET /api/v1/web/certificate` reports it.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CertificateStatus {
    /// Whether apid is serving HTTPS with it now. When not, a stored identity
    /// is what HTTPS will serve once it is enabled.
    pub(super) serving: bool,
    /// The stored identity's leaf; `null` before apid has ever served HTTPS
    /// and nothing was uploaded or generated.
    pub(super) certificate: Option<CertificateInfo>,
}

/// `PUT /api/v1/web/certificate` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CertificateUpload {
    /// The certificate chain, PEM, leaf first.
    pub(super) certificate: String,
    /// Its private key, PEM (PKCS#8, PKCS#1 or SEC1).
    pub(super) private_key: String,
}

/// `POST /api/v1/web/certificate/generate` request body.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CertificateGeneration {
    /// The subject's common name.
    pub(super) common_name: String,
    /// DNS names the certificate covers (at most 16).
    #[serde(default)]
    pub(super) dns_names: Vec<String>,
    /// IP addresses the certificate covers (at most 16).
    #[serde(default)]
    pub(super) ip_addresses: Vec<String>,
    /// Days it stays valid, 1 to 3650.
    pub(super) validity_days: u32,
}

/// Most names of either kind a generated certificate carries.
const MAX_GENERATED_NAMES: usize = 16;

/// Read the console's TLS identity. The private key is never returned.
#[utoipa::path(
    get,
    path = V1_WEB_CERTIFICATE_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The stored identity's leaf and whether HTTPS serves it now", body = CertificateStatus),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The stored identity could not be read (`identity_unreadable`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_web_certificate_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.identity.describe() {
        Ok(certificate) => api_response(
            StatusCode::OK,
            CertificateStatus {
                serving: state.identity.is_serving(),
                certificate,
            },
        ),
        Err(err) => api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid("identity_unreadable", err.to_string()),
        ),
    }
}

/// Replace the console's TLS identity with an uploaded certificate and key.
///
/// The chain must be PEM with the leaf first, the key must be the leaf's, and
/// the leaf must be inside its validity period. While HTTPS is on the new
/// identity serves from the next connection, with no restart.
#[utoipa::path(
    put,
    path = V1_WEB_CERTIFICATE_PATH,
    context_path = API,
    tag = "resources",
    request_body = CertificateUpload,
    responses(
        (status = 200, description = "The identity was stored (and reloaded when HTTPS is on); the body describes it", body = CertificateInfo),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "Not a PEM chain and key, a key that is not the certificate's, or a certificate outside its validity (`validation_failed`)", body = ApiError),
        (status = 500, description = "The identity could not be stored or reloaded (`identity_write_failed`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_web_certificate_upload(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let upload: CertificateUpload = match json_body(body, None) {
        Ok(upload) => upload,
        Err(response) => return *response,
    };
    let identity = match crate::tls::identity_from_upload(&upload.certificate, &upload.private_key)
    {
        Ok(identity) => identity,
        Err(message) => {
            state.audit.record(CERTIFICATE_EVENT, "refused", &source);
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", message),
            );
        }
    };
    store_identity(&state, &identity, "uploaded", &source).await
}

/// Replace the console's TLS identity with a new self-signed one.
///
/// A fresh key, and a certificate issued to the names given: the device's
/// hostname and addresses, so a browser told to trust it once stops warning.
#[utoipa::path(
    post,
    path = V1_WEB_CERTIFICATE_GENERATE_PATH,
    context_path = API,
    tag = "actions",
    request_body = CertificateGeneration,
    responses(
        (status = 200, description = "The identity was generated and stored (and reloaded when HTTPS is on); the body describes it", body = CertificateInfo),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "An empty common name, a name that is not a DNS name, an address that is not an IP address, too many names, or a validity outside 1 to 3650 days (`validation_failed`)", body = ApiError),
        (status = 500, description = "The identity could not be generated, stored or reloaded (`identity_write_failed`)", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_web_certificate_generate(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let generation: CertificateGeneration = match json_body(body, None) {
        Ok(generation) => generation,
        Err(response) => return *response,
    };
    let request = match certificate_request(generation) {
        Ok(request) => request,
        Err(message) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", message),
            );
        }
    };
    let identity = match crate::tls::generate_identity(&request) {
        Ok(identity) => identity,
        Err(err) => {
            return api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid("identity_write_failed", err.to_string()),
            );
        }
    };
    store_identity(&state, &identity, "generated", &source).await
}

/// The audit event of every identity change.
const CERTIFICATE_EVENT: &str = "tls-certificate";

async fn store_identity(state: &AppState, identity: &str, how: &str, source: &str) -> Response {
    match state.identity.replace(identity).await {
        Ok(info) => {
            state.audit.record(CERTIFICATE_EVENT, how, source);
            api_response(StatusCode::OK, info)
        }
        Err(err) => {
            state.audit.record(CERTIFICATE_EVENT, "failed", source);
            api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid("identity_write_failed", err.to_string()),
            )
        }
    }
}

/// Check a generation request against what a certificate can carry.
fn certificate_request(
    generation: CertificateGeneration,
) -> Result<crate::tls::CertificateRequest, String> {
    let common_name = generation.common_name.trim().to_string();
    if common_name.is_empty() || common_name.len() > 64 {
        return Err("the common name is 1 to 64 characters".to_string());
    }
    if generation.dns_names.len() > MAX_GENERATED_NAMES
        || generation.ip_addresses.len() > MAX_GENERATED_NAMES
    {
        return Err(format!(
            "a certificate carries at most {MAX_GENERATED_NAMES} DNS names and {MAX_GENERATED_NAMES} addresses"
        ));
    }
    let mut dns_names = Vec::new();
    for name in generation.dns_names {
        let name = name.trim().to_ascii_lowercase();
        if !is_dns_name(&name) {
            return Err(format!("`{name}` is not a DNS name"));
        }
        dns_names.push(name);
    }
    let mut ip_addresses = Vec::new();
    for address in generation.ip_addresses {
        let parsed = address
            .trim()
            .parse::<std::net::IpAddr>()
            .map_err(|_| format!("`{address}` is not an IPv4 or IPv6 address"))?;
        ip_addresses.push(parsed);
    }
    if dns_names.is_empty() && ip_addresses.is_empty() {
        return Err(
            "name at least one DNS name or address a browser reaches the device by".to_string(),
        );
    }
    if !(1..=3650).contains(&generation.validity_days) {
        return Err("the validity is 1 to 3650 days".to_string());
    }
    Ok(crate::tls::CertificateRequest {
        common_name,
        dns_names,
        ip_addresses,
        validity_days: Some(generation.validity_days),
    })
}

/// A DNS name a certificate may carry: labels of letters, digits and inner
/// hyphens, a leading `*.` wildcard allowed, 253 characters at most.
fn is_dns_name(name: &str) -> bool {
    let name = name.strip_prefix("*.").unwrap_or(name);
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
