//! The `/api` router: every route, mounted by the product's features.

use axum::Router;
use axum::extract::OriginalUri;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use axum::routing::delete;
use axum::routing::put;
use axum::routing::{get, post};

use super::*;

/// The reserved `/api` subtree: the declared routes, and the not-found
/// handler every other path under the prefix reaches.
///
/// Each route is declared here from the same constant its `utoipa::path`
/// attribute documents it under, so the served path and the documented path
/// are one string and cannot disagree.
pub(super) fn api_router(features: &micad_settings::Features) -> Router<AppState> {
    use micad_settings::Feature;
    // A feature the product does not carry is not mounted: its paths answer
    // the subtree's own 404, like any path the API does not serve.
    let mut router = Router::new();
    for (feature, routes) in [
        (Feature::Ssh, ssh_routes as fn() -> Router<AppState>),
        (Feature::Wifi, wifi_routes),
        (Feature::Bluetooth, bluetooth_routes),
        (Feature::Containers, container_routes),
        (Feature::Mqtt, mqtt_routes),
    ] {
        if features.has(feature) {
            router = router.merge(routes());
        }
    }
    router
        .route(VERSIONS_PATH, get(api_versions))
        .route(V1_META_PATH, get(api_v1_meta))
        .route(
            V1_SESSION_PATH,
            get(api_v1_session_status)
                .post(api_v1_session_create)
                .delete(api_v1_session_delete),
        )
        .route(V1_CLAIM_PATH, get(api_v1_claim))
        .route(V1_UI_PATH, get(api_v1_ui_status))
        .route(
            V1_UI_BUNDLES_PATH,
            get(api_v1_ui_bundles).post(api_v1_ui_upload),
        )
        .route(V1_UI_BUNDLE_ROUTE, delete(api_v1_ui_delete))
        .route(
            V1_UI_ACTIVE_PATH,
            put(api_v1_ui_activate).delete(api_v1_ui_deactivate),
        )
        .route(V1_HEALTH_PATH, get(api_v1_health))
        .route(
            V1_SETTINGS_ROUTE,
            get(api_v1_settings).put(api_v1_settings_write),
        )
        .route(V1_STATE_ROUTE, get(api_v1_state))
        .route(V1_WEB_PATH, get(api_v1_web_read).put(api_v1_web_write))
        .route(
            V1_WEB_CERTIFICATE_PATH,
            get(api_v1_web_certificate_read).put(api_v1_web_certificate_upload),
        )
        .route(
            V1_WEB_CERTIFICATE_GENERATE_PATH,
            post(api_v1_web_certificate_generate),
        )
        // GET only: the status is observed, and pausing synchronization is a
        // control this API deliberately does not have.
        .route(V1_TIME_STATUS_PATH, get(api_v1_time_status))
        // GET only, and alone: the layout is fixed, so there is no verb here
        // that could format or repartition anything.
        .route(V1_STORAGE_STATUS_PATH, get(api_v1_storage_status))
        .route(V1_SYSTEM_INFO_PATH, get(api_v1_system_info))
        .route(V1_SYSTEM_TELEMETRY_PATH, get(api_v1_system_telemetry))
        .route(V1_SYSTEM_LOG_ROUTE, get(api_v1_system_log))
        .route(V1_NETWORK_STATUS_PATH, get(api_v1_network_status))
        .route(
            V1_DIAGNOSTICS_SNAPSHOTS_PATH,
            get(api_v1_diagnostics_list).post(api_v1_diagnostics_collect),
        )
        .route(
            V1_DIAGNOSTICS_SNAPSHOT_ROUTE,
            get(api_v1_diagnostics_snapshot).delete(api_v1_diagnostics_delete),
        )
        .route(V1_TASKS_PATH, get(api_v1_tasks_list))
        .route(V1_TASK_ROUTE, get(api_v1_task))
        .route(V1_CHANGE_PASSWORD_PATH, post(api_v1_change_password))
        // Token lifecycle. Automation and authenticated browser sessions use
        // the same JSON routes. Session mutations also require CSRF.
        .route(
            V1_TOKENS_PATH,
            get(api_v1_tokens_list).post(api_v1_tokens_mint),
        )
        .route(V1_TOKEN_ROUTE, delete(api_v1_tokens_revoke))
        // Array resources share the same credential extractor as the rest of
        // the management API.
        // The network cluster. Typed rather than a dot-path passthrough
        // because the rules under `network` are relational: a bridge port has to
        // name a declared entry, which no check confined to the entry being
        // written could see. `PUT /api/v1/settings/network...` is refused at
        // 409 by [`settings_write_refusal`] and names these routes.
        .route(
            V1_NETWORK_PATH,
            get(api_v1_network_read).put(api_v1_network_write),
        )
        .route(
            V1_NETWORK_IFACE_ROUTE,
            put(api_v1_network_iface_write).delete(api_v1_network_iface_remove),
        )
        .route(
            V1_NETWORK_PEERS_ROUTE,
            get(api_v1_peers_list).post(api_v1_peers_add),
        )
        .route(V1_NETWORK_PEER_ROUTE, delete(api_v1_peers_remove))
        // POST only, for the reason the power and SSH mutations are: no GET
        // handler exists, so nothing that merely follows a link can replace a
        // tunnel's identity.
        .route(V1_WIREGUARD_ROTATE_ROUTE, post(api_v1_wireguard_rotate))
        // The update cluster (`update_api.rs`): one state read, seven
        // POST-only actions, all behind the same credential extractor.
        .route(
            crate::update_api::V1_UPDATE_PATH,
            get(crate::update_api::api_v1_update_state),
        )
        .route(
            crate::update_api::V1_UPDATE_CHECK_PATH,
            post(crate::update_api::api_v1_update_check),
        )
        .route(
            crate::update_api::V1_UPDATE_FETCH_PATH,
            post(crate::update_api::api_v1_update_fetch),
        )
        .route(
            crate::update_api::V1_UPDATE_INSTALL_PATH,
            post(crate::update_api::api_v1_update_install),
        )
        .route(
            crate::update_api::V1_UPDATE_CONFIRM_PATH,
            post(crate::update_api::api_v1_update_confirm),
        )
        .route(
            crate::update_api::V1_UPDATE_REJECT_PATH,
            post(crate::update_api::api_v1_update_reject),
        )
        .route(
            crate::update_api::V1_UPDATE_ROLLBACK_PATH,
            post(crate::update_api::api_v1_update_rollback),
        )
        .route(
            crate::update_api::V1_UPDATE_REBOOT_OVERRIDE_PATH,
            post(crate::update_api::api_v1_update_reboot_override),
        )
        .route(
            crate::update_api::V1_UPDATE_CONFIG_PATH,
            post(crate::update_api::api_v1_update_config),
        )
        .route(
            crate::update_api::V1_UPDATE_IMPORT_PATH,
            post(crate::update_api::api_v1_update_import),
        )
        // Actions are POST-only so navigation and prefetch cannot trigger
        // state changes.
        .route(V1_REBOOT_PATH, post(api_v1_reboot))
        .route(V1_POWEROFF_PATH, post(api_v1_poweroff))
        .route(
            V1_TRANSIENT_PASSWORD_PATH,
            post(api_v1_transient_root_password),
        )
        // First-run setup is the only unauthenticated write and remains
        // POST-only.
        .route(V1_SETUP_PATH, post(api_v1_setup))
        // GET only: what a provisioning document did is observed; applying one
        // is a file on a medium, read before anything is listening.
        .route(
            V1_PROVISIONING_STATUS_PATH,
            get(crate::provisioning_api::api_v1_provisioning_status),
        )
        // POST only, for the reason the power actions are: no GET handler
        // exists, so nothing that merely follows a link can stage a reset.
        .route(V1_RESET_PATH, post(api_v1_reset))
        // POST only, and with no credential extractor: the authority is
        // physical presence, and an authenticated session is refused rather
        // than admitted.
        .route(
            V1_RECOVERY_CREDENTIAL_PATH,
            post(api_v1_recovery_credential),
        )
        // The envelope on the methods those routes do not serve, declared
        // once for the subtree rather than route by route. It reaches exactly
        // the routes above — it rewrites the method-not-allowed fallback of
        // every `MethodRouter` already registered on *this* router. It must
        // stay below the last `.route`: a route declared after it would not be
        // reached.
        .method_not_allowed_fallback(api_method_not_allowed)
        .fallback(api_not_found)
}

/// SSH keys (`ssh`).
pub(super) fn ssh_routes() -> Router<AppState> {
    Router::new()
        .route(
            V1_SSH_KEYS_PATH,
            get(api_v1_ssh_keys_list).post(api_v1_ssh_keys_add),
        )
        .route(V1_SSH_KEY_ROUTE, delete(api_v1_ssh_keys_remove))
}

/// The Wi-Fi station and access point (`wifi`).
pub(super) fn wifi_routes() -> Router<AppState> {
    Router::new()
        .route(
            V1_WIFI_NETWORKS_PATH,
            get(api_v1_wifi_networks_list).post(api_v1_wifi_networks_add),
        )
        .route(
            V1_WIFI_NETWORK_ROUTE,
            put(api_v1_wifi_networks_replace).delete(api_v1_wifi_networks_remove),
        )
        .route(
            V1_WIFI_CLIENT_PATH,
            get(api_v1_wifi_client_read).put(api_v1_wifi_client_write),
        )
        .route(V1_WIFI_SCAN_PATH, post(api_v1_wifi_scan))
        .route(
            V1_WIFI_AP_PATH,
            get(api_v1_wifi_ap_read).put(api_v1_wifi_ap_write),
        )
}

/// Bluetooth: one document, one write for the adapter, and the verbs that pair
/// (`bluetooth`).
pub(super) fn bluetooth_routes() -> Router<AppState> {
    Router::new()
        .route(
            V1_BLUETOOTH_PATH,
            get(api_v1_bluetooth_read).put(api_v1_bluetooth_write),
        )
        .route(
            V1_BLUETOOTH_DISCOVERY_PATH,
            post(api_v1_bluetooth_discovery),
        )
        .route(
            V1_BLUETOOTH_DEVICE_ROUTE,
            delete(api_v1_bluetooth_device_remove),
        )
        .route(
            V1_BLUETOOTH_DEVICE_ACTION_ROUTE,
            post(api_v1_bluetooth_device_action),
        )
}

/// The containers: declared as settings, observed through the engine, driven
/// as units (`containers`).
pub(super) fn container_routes() -> Router<AppState> {
    Router::new()
        .route(V1_CONTAINERS_PATH, get(api_v1_containers_read))
        .route(
            V1_CONTAINER_ROUTE,
            put(api_v1_container_write).delete(api_v1_container_remove),
        )
        .route(V1_CONTAINER_ACTION_ROUTE, post(api_v1_container_action))
}

/// One document for the broker and the bridge: the declared subtree and the
/// reconciler's own report of what it did with it (`mqtt`).
pub(super) fn mqtt_routes() -> Router<AppState> {
    Router::new().route(V1_MQTT_PATH, get(api_v1_mqtt_read).put(api_v1_mqtt_write))
}

/// The envelope for a method a declared route does not serve.
///
/// There is **one** shape for every failure on every `/api/v1/` route, and a
/// wrong method is a failure like any other. Without this the answer is axum's
/// own: a bare 405 with no body and no `Content-Type` at all — measured — so a
/// client that parses the error envelope on every other failure had nothing to parse
/// on this one.
///
/// The `Allow` header is left to axum deliberately. axum accumulates it from
/// the very `get`/`post` calls that declare each route above and attaches it to
/// whatever this handler returns unless the response already carries one, so
/// the header cannot name a method a route does not serve or omit one it does.
/// A hand-written `Allow` here would be a second opinion about the route table,
/// and second opinions drift.
///
/// `source` is `"apid"`: the router made this decision and no bus call was
/// made, so there is nothing micad could be asked about it. There is no `path`
/// member for the same reason the not-found envelope has none — a wrong method
/// names no settings dot-path.
///
/// It is reached without an authentication check, which is what the shipped
/// tree already did: [`is_declared_api_route`] tests the path and not the
/// method, so the gate hands a wrong-method request on a declared path off just
/// as it hands off the right one. The status is 405 either way; this changes
/// what is in the body, not who may see it.
pub(super) async fn api_method_not_allowed(
    method: Method,
    OriginalUri(uri): OriginalUri,
) -> Response {
    api_response(
        StatusCode::METHOD_NOT_ALLOWED,
        ApiError::apid(
            "method_not_allowed",
            format!("{method} is not a method {} serves", uri.path()),
        ),
    )
}
