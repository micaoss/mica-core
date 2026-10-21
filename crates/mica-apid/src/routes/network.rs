//! Network interfaces.

use crate::redact;
use axum::Json;
use axum::extract::{Path, Query, State};
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
        (status = 200, description = "Configured interfaces, the stored map's version and the current systemd-networkd observation", body = NetworkOverview),
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
    let (configured, version) = match state.api.get_settings(NETWORK_SETTINGS_PATH).await {
        Ok(value) => {
            let version = network_version(&value);
            (redact::redact(value, NETWORK_SETTINGS_PATH), version)
        }
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
            version,
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
/// The body names the version `GET` answered: a map changed since then is
/// **409** `version_conflict`, and so is any write while a network task has
/// not finished (`busy`). A map that does not validate is **422**: `code`,
/// `message` and `params` are the first broken rule's, and `details` lists
/// every one by interface and field. Nothing is written in either case.
/// `?dryRun=true` validates and answers what the write would change.
#[utoipa::path(
    put,
    path = V1_NETWORK_PATH,
    context_path = API,
    tag = "resources",
    params(NetworkWriteQuery),
    request_body = NetworkMapWrite,
    responses(
        (status = 200, description = "A dry run: what the write would change; nothing was written", body = NetworkDryRun),
        (status = 202, description = "The map was replaced and the reconcile queued; the body carries the task id and the new version", body = NetworkAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "`baseVersion` is not the stored map's version (`version_conflict`), or a network task has not finished (`busy`)", body = ApiError),
        (status = 422, description = "`dryRun` is not a boolean or the body is not a version and a map of interfaces (`validation_failed`); or the map breaks a rule, each in `details` with its own code: a key that is not an interface name (`name_invalid`), two overlapping subnets (`address_overlap`), a gateway outside its subnet (`gateway_out_of_subnet`), two default routes with one metric (`metric_duplicate`), one VLAN id twice on a parent (`vlan_duplicate`), a VLAN parent that is not a declared entry or is a CAN one (`vlan_parent_invalid`), a bridge port that is not a declared entry or is a CAN one (`bridge_port_invalid`), carries addressing (`bridge_port_addressed`) or is claimed twice (`bridge_port_taken`), and a field's own syntax (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    query: Result<Query<NetworkWriteQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let dry_run = match query {
        Ok(Query(query)) => query.dry_run.unwrap_or(false),
        Err(rejection) => {
            return api_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiError::apid("validation_failed", rejection.body_text())
                    .at(NETWORK_SETTINGS_PATH),
            );
        }
    };
    let NetworkMapWrite {
        base_version,
        configured: mut entries,
    } = match json_body(body, Some(NETWORK_SETTINGS_PATH)) {
        Ok(write) => write,
        Err(response) => return *response,
    };
    let stored = match state.api.get_settings(NETWORK_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(NETWORK_SETTINGS_PATH)),
    };
    // The check and the write are two bus calls, so a write landing between
    // them is not caught.
    if network_version(&stored) != base_version {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "version_conflict",
                "the network map changed since it was read; read it again and reapply the changes"
                    .to_string(),
            )
            .at(NETWORK_SETTINGS_PATH),
        );
    }
    if !dry_run && let Err(response) = check_network_idle(&state).await {
        return *response;
    }
    if carries_redacted_psk(entries.values()) {
        let stored = match api_network_entries(&state).await {
            Ok(stored) => stored,
            Err(response) => return *response,
        };
        for (iface, cfg) in &mut entries {
            if let Err(response) = restore_preshared_keys(iface, cfg, &stored) {
                return *response;
            }
        }
    }
    // Every entry here is one the request carried: this route replaces the
    // map rather than merging into it. The keys are checked as well as the
    // bodies: `Settings::set` checks a new `network` key itself, but only
    // after the write is built.
    let carried: Vec<&str> = entries.keys().map(String::as_str).collect();
    if let Err(response) = map_refusal(&entries, &carried, NETWORK_SETTINGS_PATH) {
        return *response;
    }
    if dry_run {
        // Entries this build cannot read count as removed: the write drops them.
        let (current, _unreadable) = parse_network(&stored);
        return api_response(
            StatusCode::OK,
            NetworkDryRun {
                adopted: Vec::new(),
                warnings: Vec::new(),
                diff: network_diff(&current, &entries),
            },
        );
    }
    let task_id = match write_network_map(&state, &entries).await {
        Ok(task_id) => task_id,
        Err(response) => return *response,
    };
    match state.api.get_settings(NETWORK_SETTINGS_PATH).await {
        Ok(value) => api_response(
            StatusCode::ACCEPTED,
            NetworkAccepted {
                task_id,
                version: network_version(&value),
            },
        ),
        Err(err) => bus_api_error(&err, Some(NETWORK_SETTINGS_PATH)),
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
        (status = 409, description = "A network task has not finished (`busy`)", body = ApiError),
        (status = 422, description = "The name is not an interface name, or the body is not an interface (`validation_failed`); or the resulting map breaks a rule, each in `details` with the code `PUT /api/v1/network` answers it with; or micad rejected the write (`settings_rejected`)", body = ApiError),
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
    let mut cfg: IfaceSettings = match json_body(body, Some(&path)) {
        Ok(cfg) => cfg,
        Err(response) => return *response,
    };
    if let Err(response) = check_network_idle(&state).await {
        return *response;
    }
    let mut candidate = match api_network_entries(&state).await {
        Ok(entries) => entries,
        Err(response) => return *response,
    };
    if let Err(response) = restore_preshared_keys(&iface, &mut cfg, &candidate) {
        return *response;
    }
    // The candidate tree, for the relational rules, which are about two
    // entries at once; the entry's own rules for the one entry the request
    // carries: a stored entry that already holds an unparseable address is
    // not this request's fault and must not make an edit to a different
    // interface fail.
    candidate.insert(iface.clone(), cfg.clone());
    if let Err(response) = map_refusal(&candidate, &[iface.as_str()], &path) {
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
/// bridge, a VLAN's parent — is **422** `in_use` naming them, and nothing is
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
        (status = 409, description = "A network task has not finished (`busy`)", body = ApiError),
        (status = 422, description = "The name is not an interface name (`validation_failed`), or a bridge still lists it as a port or a VLAN still names it as a parent (`in_use`, with `refs`)", body = ApiError),
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
    if let Err(response) = check_network_idle(&state).await {
        return *response;
    }
    let refs = references_to(&candidate, &iface);
    if !refs.is_empty() {
        let refs = refs.join(", ");
        let message = format!("{iface} is named by {refs}; remove those references first");
        let violation = NetworkViolation::new(
            &iface,
            "name",
            "in_use",
            serde_json::json!({ "name": iface, "refs": refs }),
            message,
        );
        if let Err(response) = violations_refusal(vec![violation], &path) {
            return *response;
        }
    }
    if let Err(response) = relational_refusal(&candidate, &path) {
        return *response;
    }
    if let Err(response) = write_network_map(&state, &candidate).await {
        return *response;
    }
    no_content()
}
