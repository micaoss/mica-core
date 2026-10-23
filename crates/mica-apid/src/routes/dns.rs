//! The DNS module: what the device resolves with, the global servers, the
//! static records by id, the server test and the resolution check.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::{DnsSettings, GlobalDns, IfaceSettings, StaticRecord};
use serde_json::{Value, json};

use super::*;

/// The global servers, as read and as written.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsGlobal {
    /// Whether the servers replace every interface's.
    pub(super) enabled: bool,
    /// One to three IPv4 servers while on; kept while off.
    pub(super) servers: Vec<String>,
}

/// One static record.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct DnsRecord {
    /// What the item routes name it by.
    pub(super) id: String,
    /// The host name, a single label or dotted.
    pub(super) hostname: String,
    /// The IPv4 address it resolves to.
    pub(super) address: String,
    /// A note for people, at most 64 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) comment: Option<String>,
}

/// A static record as a client writes it; the id is the device's.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsRecordBody {
    /// The host name, a single label or dotted.
    pub(super) hostname: String,
    /// The IPv4 address it resolves to.
    pub(super) address: String,
    /// A note for people, at most 64 characters.
    #[serde(default)]
    pub(super) comment: Option<String>,
}

/// One interface's servers, when each interface uses its own.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct DnsLink {
    /// The interface.
    pub(super) name: String,
    /// The servers it reports, possibly none.
    pub(super) servers: Vec<String>,
    /// Where they come from: `dhcp` (the lease), `custom` (set on a DHCP
    /// interface) or `static`.
    pub(super) source: &'static str,
}

/// The servers lookups actually go to: the global list, or each interface's.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct DnsEffective {
    /// The global servers, in global mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) global: Option<Vec<String>>,
    /// Each observed interface and its servers, in interface mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) links: Option<Vec<DnsLink>>,
}

/// The last resolution check.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DnsProbe {
    /// Whether the probe name resolved.
    pub(super) ok: bool,
    /// The name looked up.
    pub(super) name: String,
    /// How long it took, in milliseconds, when it resolved.
    pub(super) latency_ms: Option<u64>,
    /// When it finished, RFC 3339 UTC.
    pub(super) checked_at: String,
    /// Why it did not resolve.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
}

/// The DNS document.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DnsDocument {
    /// The global servers.
    pub(super) global: DnsGlobal,
    /// `global` when the global servers replace every interface's, otherwise
    /// `interface`.
    pub(super) mode: &'static str,
    /// The servers lookups go to.
    pub(super) effective: DnsEffective,
    /// The names the device resolves itself.
    pub(super) static_records: Vec<DnsRecord>,
    /// The last resolution check; `null` before the first.
    #[schema(value_type = Option<DnsProbe>)]
    pub(super) probe: Value,
}

fn record_out(record: StaticRecord) -> DnsRecord {
    DnsRecord {
        id: record.id,
        hostname: record.hostname,
        address: record.address,
        comment: record.comment,
    }
}

/// The stored `dns` subtree.
async fn stored_dns(state: &AppState) -> Result<DnsSettings, Box<Response>> {
    let value = state
        .api
        .get_settings(DNS_PATH)
        .await
        .map_err(|err| Box::new(bus_api_error(&err, Some(DNS_PATH))))?;
    serde_json::from_value(value).map_err(|err| {
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "settings_invalid",
                format!("the stored DNS settings could not be read: {err}"),
            )
            .at(DNS_PATH),
        ))
    })
}

/// The stored interface map; an unreadable one is empty, since it only
/// names where servers come from and which addresses are the device's own.
async fn stored_network(state: &AppState) -> BTreeMap<String, IfaceSettings> {
    match state.api.get_settings("network").await {
        Ok(value) => serde_json::from_value(value).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    }
}

/// Where a declared interface's servers come from.
fn link_source(entry: &IfaceSettings) -> &'static str {
    if entry.dhcp && !entry.dns.is_empty() {
        "custom"
    } else if !entry.dhcp && entry.static_.is_some() {
        "static"
    } else {
        "dhcp"
    }
}

/// Each observed interface that takes part in resolution, with the servers it
/// reports: one with carrier that has servers or runs DHCP to bring them. A
/// bridge port, an access point's radio, a CAN or dummy link is not one.
fn observed_links(observed: &Value, network: &BTreeMap<String, IfaceSettings>) -> Vec<DnsLink> {
    let port = |name: &str| {
        network.values().any(|entry| {
            entry
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.ports.iter().any(|port| port == name))
        })
    };
    observed["interfaces"]["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = entry["name"].as_str()?;
            let servers: Vec<String> = entry["dns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|server| server.as_str().map(str::to_string))
                .collect();
            let dhcp = match network.get(name) {
                Some(declared) => declared.dhcp && !port(name),
                None => {
                    entry["lease"]["available"] == true
                        || entry["addresses"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|address| address["configSource"] == "DHCPv4")
                }
            };
            let resolves =
                name != "lo" && entry["link"]["carrier"] == true && (dhcp || !servers.is_empty());
            resolves.then(|| DnsLink {
                name: name.to_string(),
                servers,
                source: match network.get(name) {
                    Some(declared) => link_source(declared),
                    None if dhcp => "dhcp",
                    None => "static",
                },
            })
        })
        .collect()
}

/// Read what the device resolves with.
///
/// The global servers and the static records are the settings; `effective`
/// is the global list in global mode and otherwise what each interface taking
/// part in resolution reports (with carrier, and servers or DHCP to bring
/// them), with where its servers come from; `probe` is the last
/// resolution check, which runs every 60 seconds.
#[utoipa::path(
    get,
    path = V1_DNS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The DNS document", body = DnsDocument),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The stored DNS settings could not be read (`settings_invalid`), or micad failed to answer (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let dns = match stored_dns(&state).await {
        Ok(dns) => dns,
        Err(response) => return *response,
    };
    let effective = if dns.global.enabled {
        DnsEffective {
            global: Some(dns.global.servers.clone()),
            links: None,
        }
    } else {
        let network = stored_network(&state).await;
        let observed = state.api.get_observed_network().await.unwrap_or_default();
        DnsEffective {
            global: None,
            links: Some(observed_links(&observed, &network)),
        }
    };
    let probe = state.api.get_dns_probe().await.unwrap_or(Value::Null);
    api_response(
        StatusCode::OK,
        DnsDocument {
            mode: if dns.global.enabled {
                "global"
            } else {
                "interface"
            },
            global: DnsGlobal {
                enabled: dns.global.enabled,
                servers: dns.global.servers,
            },
            effective,
            static_records: dns.static_records.into_iter().map(record_out).collect(),
            probe,
        },
    )
}

/// The first server that is this device itself: loopback, unspecified, or an
/// address an interface declares or holds.
fn own_server<'a>(
    servers: &'a [String],
    network: &BTreeMap<String, IfaceSettings>,
    observed: &Value,
) -> Option<&'a String> {
    let mut own: BTreeSet<String> = network
        .values()
        .filter_map(|cfg| cfg.static_.as_ref())
        .filter_map(|static_| static_.address.split('/').next().map(str::to_string))
        .collect();
    for entry in observed["interfaces"]["entries"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for address in entry["addresses"].as_array().into_iter().flatten() {
            if let Some(address) = address["address"].as_str() {
                own.insert(address.split('/').next().unwrap_or(address).to_string());
            }
        }
    }
    servers.iter().find(|server| {
        server
            .parse::<Ipv4Addr>()
            .is_ok_and(|addr| addr.is_loopback() || addr.is_unspecified())
            || own.contains(server.as_str())
    })
}

/// Switch the global servers on or off, or change them.
///
/// On needs one to three IPv4 servers, none of them this device and none
/// twice; off keeps the list. Applied as a task: resolved and every
/// interface's servers change with it.
#[utoipa::path(
    put,
    path = V1_DNS_GLOBAL_PATH,
    context_path = API,
    tag = "resources",
    request_body = DnsGlobal,
    responses(
        (status = 202, description = "Stored; the reconcile continues as a task", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "A server is loopback or one of this device's addresses (`dns_server_invalid`), or the body breaks another rule (`validation_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_global_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let global: DnsGlobal = match json_body(body, Some(DNS_GLOBAL_PATH)) {
        Ok(global) => global,
        Err(response) => return *response,
    };
    let network = stored_network(&state).await;
    let observed = state.api.get_observed_network().await.unwrap_or_default();
    if let Some(server) = own_server(&global.servers, &network, &observed) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "dns_server_invalid",
                format!("{server} is this device itself, not a server it can ask"),
            )
            .with_params(json!({ "server": server }))
            .at(DNS_GLOBAL_PATH),
        );
    }
    let candidate = DnsSettings {
        global: GlobalDns {
            enabled: global.enabled,
            servers: global.servers,
        },
        ..DnsSettings::default()
    };
    if let Err(message) = micad_settings::validate_dns(&candidate, "", &network) {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(DNS_GLOBAL_PATH),
        );
    }
    let value = match encode(&candidate.global) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(DNS_GLOBAL_PATH, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(DNS_GLOBAL_PATH)),
    }
}

/// A record body checked on its own: the address IPv4, the name not taken by
/// another record. Answers the refusal.
fn record_refusal(
    records: &[StaticRecord],
    body: &DnsRecordBody,
    except: Option<&str>,
) -> Option<Response> {
    if body.address.parse::<Ipv4Addr>().is_err() {
        return Some(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                format!("{:?} is not an IPv4 address", body.address),
            )
            .at(DNS_RECORDS_PATH),
        ));
    }
    let taken = records.iter().any(|record| {
        Some(record.id.as_str()) != except && record.hostname.eq_ignore_ascii_case(&body.hostname)
    });
    taken.then(|| {
        api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "record_duplicate",
                format!("a record already names {}", body.hostname),
            )
            .with_params(json!({ "hostname": body.hostname }))
            .at(DNS_RECORDS_PATH),
        )
    })
}

/// Store `records`: refused here first by the rule micad holds them to, so
/// the answer is this route's; written synchronously, as micad persists a
/// write before it answers, and applied by rewriting the hosts file only.
async fn write_records(state: &AppState, records: Vec<StaticRecord>) -> Result<(), Box<Response>> {
    let hostname = state
        .api
        .get_settings("hostname")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let candidate = DnsSettings {
        static_records: records,
        ..DnsSettings::default()
    };
    if let Err(message) = micad_settings::validate_dns(&candidate, &hostname, &BTreeMap::new()) {
        return Err(Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid("validation_failed", message).at(DNS_RECORDS_PATH),
        )));
    }
    let value = encode(&candidate.static_records)?;
    state
        .api
        .set_settings(DNS_RECORDS_PATH, &value)
        .await
        .map(drop)
        .map_err(|err| Box::new(bus_api_error(&err, Some(DNS_RECORDS_PATH))))
}

/// A fresh record id: eight hex digits no record carries.
fn fresh_id(records: &[StaticRecord]) -> String {
    loop {
        let id: String = micad_settings::random_bytes::<4>()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if !records.iter().any(|record| record.id == id) {
            return id;
        }
    }
}

/// Add a static record.
///
/// Takes effect at once: the hosts file is rewritten and no interface is
/// reloaded. The device gives the record its id.
#[utoipa::path(
    post,
    path = V1_DNS_RECORDS_PATH,
    context_path = API,
    tag = "resources",
    request_body = DnsRecordBody,
    responses(
        (status = 201, description = "The record as stored, with its id", body = DnsRecord),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "Another record names the host (`record_duplicate`), or there are 100 records already (`record_limit`)", body = ApiError),
        (status = 422, description = "The name is not a host name, is localhost or this device's, the address is not IPv4, or the comment is long (`validation_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_record_add(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body: DnsRecordBody = match json_body(body, Some(DNS_RECORDS_PATH)) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let mut records = match stored_dns(&state).await {
        Ok(dns) => dns.static_records,
        Err(response) => return *response,
    };
    if records.len() >= micad_settings::MAX_STATIC_RECORDS {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "record_limit",
                format!(
                    "there are {} records already, the most there can be",
                    micad_settings::MAX_STATIC_RECORDS
                ),
            )
            .with_params(json!({ "count": micad_settings::MAX_STATIC_RECORDS }))
            .at(DNS_RECORDS_PATH),
        );
    }
    if let Some(response) = record_refusal(&records, &body, None) {
        return response;
    }
    let record = StaticRecord {
        id: fresh_id(&records),
        hostname: body.hostname,
        address: body.address,
        comment: body.comment.filter(|comment| !comment.is_empty()),
    };
    records.push(record.clone());
    if let Err(response) = write_records(&state, records).await {
        return *response;
    }
    api_response(StatusCode::CREATED, record_out(record))
}

/// Replace one static record.
#[utoipa::path(
    put,
    path = V1_DNS_RECORD_ROUTE,
    context_path = API,
    tag = "resources",
    params(("id" = String, Path, description = "The record's id, as `GET /api/v1/dns` returns it")),
    request_body = DnsRecordBody,
    responses(
        (status = 200, description = "The record as stored", body = DnsRecord),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No record has that id (`settings_not_found`)", body = ApiError),
        (status = 409, description = "Another record names the host (`record_duplicate`)", body = ApiError),
        (status = 422, description = "The record breaks a rule (`validation_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_record_replace(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let body: DnsRecordBody = match json_body(body, Some(DNS_RECORDS_PATH)) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let mut records = match stored_dns(&state).await {
        Ok(dns) => dns.static_records,
        Err(response) => return *response,
    };
    let Some(index) = records.iter().position(|record| record.id == id) else {
        return item_not_found(DNS_RECORDS_PATH, &id);
    };
    if let Some(response) = record_refusal(&records, &body, Some(&id)) {
        return response;
    }
    let record = StaticRecord {
        id,
        hostname: body.hostname,
        address: body.address,
        comment: body.comment.filter(|comment| !comment.is_empty()),
    };
    records[index] = record.clone();
    if let Err(response) = write_records(&state, records).await {
        return *response;
    }
    api_response(StatusCode::OK, record_out(record))
}

/// Remove one static record.
#[utoipa::path(
    delete,
    path = V1_DNS_RECORD_ROUTE,
    context_path = API,
    tag = "resources",
    params(("id" = String, Path, description = "The record's id, as `GET /api/v1/dns` returns it")),
    responses(
        (status = 204, description = "The record is gone from the hosts file"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No record has that id (`settings_not_found`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_record_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut records = match stored_dns(&state).await {
        Ok(dns) => dns.static_records,
        Err(response) => return *response,
    };
    let Some(index) = records.iter().position(|record| record.id == id) else {
        return item_not_found(DNS_RECORDS_PATH, &id);
    };
    records.remove(index);
    if let Err(response) = write_records(&state, records).await {
        return *response;
    }
    no_content()
}

/// Run the resolution check now.
///
/// **POST**, because it sends a query off the device. The probe name is
/// looked up through the device's own resolver, bypassing its cache, within
/// three seconds; the result is also what `GET /api/v1/dns` reports next.
#[utoipa::path(
    post,
    path = V1_DNS_PROBE_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 200, description = "The check just run", body = DnsProbe),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_dns_probe(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.probe_dns().await {
        Ok(value) => api_response(StatusCode::OK, value),
        Err(err) => bus_api_error(&err, Some(DNS_PATH)),
    }
}

/// The servers to test.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DnsTestRequest {
    /// IPv4 server addresses, one to three.
    pub(super) servers: Vec<String>,
}

/// What asking one server found.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DnsTestResult {
    /// The server asked.
    pub(super) server: String,
    /// Whether it answered, whatever the answer was.
    pub(super) reachable: bool,
    /// How long the answer took, in milliseconds; absent when there was none.
    pub(super) latency_ms: Option<u64>,
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
