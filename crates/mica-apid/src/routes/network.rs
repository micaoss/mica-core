//! Network interfaces.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::IfaceSettings;
use serde_json::Value;

use super::*;

/// Read declared network configuration together with current link state.
#[utoipa::path(
    get,
    path = V1_NETWORK_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "Configured interfaces and the current systemd-networkd observation", body = NetworkOverview),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The configured network map could not be read", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let configured = match state.api.get_settings(NETWORK_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(NETWORK_SETTINGS_PATH)),
    };
    let configured_count = configured.as_object().map_or(0, serde_json::Map::len);
    let observed = match state.api.get_network_state().await {
        Ok(value) => {
            let raw_interfaces = value
                .get("interfaces")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let interfaces = raw_interfaces
                .into_iter()
                .filter_map(|interface| serde_json::from_value(interface).ok())
                .collect::<Vec<ObservedInterface>>();
            let interface_count = value
                .get("interfaceCount")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
                .unwrap_or(interfaces.len());
            ObservedNetwork {
                available: true,
                interface_count,
                interfaces,
                error: None,
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "live network observation unavailable");
            ObservedNetwork {
                available: false,
                interface_count: 0,
                interfaces: Vec::new(),
                error: Some("systemd-networkd state is currently unavailable"),
            }
        }
    };
    api_response(
        StatusCode::OK,
        NetworkOverview {
            configured,
            configured_count,
            observed,
        },
    )
}

/// Replace the whole interface map, validated as one tree.
///
/// The only way to make two interdependent entries legal in one step: adding a
/// bridge and its ports through the per-interface route means declaring the
/// ports first, because a bridge naming an undeclared port is refused.
///
/// Answers **422** with the failing rule's own message when the map does not
/// validate; nothing is written in that case.
#[utoipa::path(
    put,
    path = V1_NETWORK_PATH,
    context_path = API,
    tag = "resources",
    request_body = std::collections::BTreeMap<String, NetworkInterface>,
    responses(
        (status = 202, description = "The map was replaced and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not a map of interfaces, a key is not an interface name, an entry declares a static address that is not IPv4 CIDR notation, or a relational rule refuses it -- a VLAN parent or a bridge port that is not a declared entry, a bridge port carrying addressing, a port claimed twice (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let entries: NetworkEntries = match json_body(body, Some(NETWORK_SETTINGS_PATH)) {
        Ok(entries) => entries,
        Err(response) => return *response,
    };
    // The keys as well as the bodies. `Settings::set` checks a new
    // `network` key itself, but it checks it *after* the write is
    // built, and a 422 that says which name is wrong beats one that says the
    // tree did not deserialize.
    for iface in entries.keys() {
        if let Err(response) = check_iface_name(iface, NETWORK_SETTINGS_PATH) {
            return *response;
        }
    }
    if let Err(response) = relational_refusal(&entries, NETWORK_SETTINGS_PATH) {
        return *response;
    }
    // After the relational pass and not before it, so a body that breaks both
    // gets the relational answer. Every entry here
    // is one the request carried: this route replaces the map rather than
    // merging into it.
    for (iface, cfg) in &entries {
        if let Err(response) = address_refusal(iface, cfg) {
            return *response;
        }
        if let Err(response) = routing_refusal(iface, cfg) {
            return *response;
        }
    }
    // No read first, deliberately: this route's whole contract is that the map
    // it sends is the map that ends up stored, so a read-modify-write would be
    // reading something it is about to discard.
    match write_network_map(&state, &entries).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(response) => *response,
    }
}

// The HTML pane carries stored peers across a save and this route does not,
// because a form posts only the fields it renders while a JSON body says
// exactly what the client meant.
/// Declare or replace one interface, validated against the whole map.
///
/// **A `PUT` replaces the entry entirely**, including a tunnel's peer list: a
/// body with no `wireguard` block on an interface that had one leaves it with
/// none. To change one field, send the whole entry.
///
/// Creates the interface when it does not exist, so there is no 404. A name
/// outside the interface charset, or a map that fails validation, is **422**.
#[utoipa::path(
    put,
    path = V1_NETWORK_IFACE_ROUTE,
    context_path = API,
    tag = "resources",
    params(("iface" = String, Path, description = "The interface to declare or replace: `eth0`, or `eth0.100` for a VLAN. Created when it does not exist")),
    request_body = NetworkInterface,
    responses(
        (status = 202, description = "The entry was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The name is not an interface name, the body is not an interface, the entry declares a static address that is not IPv4 CIDR notation, or a relational rule refuses the resulting map (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_iface_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(iface): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let path = iface_settings_path(&iface);
    if let Err(response) = check_iface_name(&iface, &path) {
        return *response;
    }
    let cfg: IfaceSettings = match json_body(body, Some(&path)) {
        Ok(cfg) => cfg,
        Err(response) => return *response,
    };
    let mut candidate = match api_network_entries(&state).await {
        Ok(entries) => entries,
        Err(response) => return *response,
    };
    // The candidate tree and not the one entry, for the reason the pane's
    // comment gives: every relational rule is about two entries at once.
    candidate.insert(iface.clone(), cfg.clone());
    if let Err(response) = relational_refusal(&candidate, &path) {
        return *response;
    }
    // `cfg` and not `candidate`: the one entry the request carries. A stored
    // entry that already holds an unparseable address is not this request's
    // fault and must not make an edit to a different interface fail.
    if let Err(response) = address_refusal(&iface, &cfg) {
        return *response;
    }
    if let Err(response) = routing_refusal(&iface, &cfg) {
        return *response;
    }
    // The entry's own dot-path and not the whole map, so a concurrent edit of
    // a different interface is not lost. Infallible: `IfaceSettings` is a
    // struct of scalars, strings and vectors with no map keys to collide.
    let value = match encode(&cfg) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(&path, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(&path)),
    }
}

// The whole map is rewritten because the dot-path syntax has no delete: the
// only way to say "this key is gone" is to send the map without it.
/// Remove one interface, re-validating the rest of the map without it.
///
/// Removing an interface another entry depends on — a port still listed by a
/// bridge — is **422** with the failing rule's own message, and nothing is
/// written. Remove the dependent reference first.
///
/// Read-modify-write: two concurrent removals lose one.
#[utoipa::path(
    delete,
    path = V1_NETWORK_IFACE_ROUTE,
    context_path = API,
    tag = "resources",
    params(("iface" = String, Path, description = "The declared interface to remove")),
    responses(
        (status = 204, description = "The entry was removed; the reconciler has swept its units"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No `network` entry has that name (`settings_not_found`). Well-formed and absent, which is a different answer from malformed", body = ApiError),
        (status = 422, description = "The name is not an interface name, or removing the entry breaks a relational rule -- a bridge still lists it as a port, a VLAN still names it as a parent (`validation_failed`)", body = ApiError),
        (status = 500, description = "The stored map holds an entry this build cannot read (`settings_invalid`), or micad failed (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_iface_remove(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(iface): Path<String>,
) -> Response {
    let path = iface_settings_path(&iface);
    if let Err(response) = check_iface_name(&iface, &path) {
        return *response;
    }
    let mut candidate = match api_network_entries(&state).await {
        Ok(entries) => entries,
        Err(response) => return *response,
    };
    if candidate.remove(&iface).is_none() {
        return item_not_found(NETWORK_SETTINGS_PATH, &iface);
    }
    if let Err(response) = relational_refusal(&candidate, &path) {
        return *response;
    }
    if let Err(response) = write_network_map(&state, &candidate).await {
        return *response;
    }
    no_content()
}
