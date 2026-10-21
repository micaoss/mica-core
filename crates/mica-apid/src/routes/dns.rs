//! The DNS server test.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The servers to test.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsTestRequest {
    /// IPv4 server addresses, one to three.
    pub(super) servers: Vec<String>,
}

/// What asking one server found.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct DnsTestResult {
    /// The server asked.
    pub(super) server: String,
    /// Whether it answered, whatever the answer was.
    pub(super) reachable: bool,
    /// How long the answer took, in milliseconds; absent when there was none.
    pub(super) ms: Option<u64>,
}

/// Ask each server directly for one `A` record.
///
/// **POST**, because it sends traffic off the device. Each server gets one
/// UDP query for the probe name and three seconds to answer; they are asked
/// at once and answered in the order given. Nothing is written.
#[utoipa::path(
    post,
    path = V1_DNS_TEST_PATH,
    context_path = API,
    tag = "actions",
    request_body = DnsTestRequest,
    responses(
        (status = 200, description = "One result per server, in the order given", body = Vec<DnsTestResult>),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not this shape, names no server or more than three, or a server is not an IPv4 address (`validation_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_test(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let request: DnsTestRequest = match json_body(body, Some(DNS_GLOBAL_PATH)) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let max = micad_settings::MAX_GLOBAL_DNS_SERVERS;
    let refusal = if request.servers.is_empty() || request.servers.len() > max {
        Some(format!("test 1 to {max} servers"))
    } else {
        request
            .servers
            .iter()
            .find(|server| server.parse::<std::net::Ipv4Addr>().is_err())
            .map(|server| format!("{server:?} is not an IPv4 address"))
    };
    if let Some(message) = refusal {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(DNS_GLOBAL_PATH),
        );
    }
    match state.api.test_dns_servers(&request.servers).await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(DNS_GLOBAL_PATH)),
    }
}
