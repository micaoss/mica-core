//! The MQTT broker and bridge.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use super::*;

/// The `mqtt` subtree as the device holds it.
///
/// Every field is required on a write: a `PUT` replaces the subtree, so a body
/// that omitted the listener would have to be merged with something, and the
/// only honest something is the value the client last read.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MqttConfiguration {
    /// Whether the broker and the bridge run at all.
    pub(super) enabled: bool,
    /// Where the broker listens.
    pub(super) listen: MqttListen,
    /// Whether the broker demands credentials.
    pub(super) auth: MqttAuth,
}

/// The broker's single listener.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MqttListen {
    /// The address the broker binds, as an IPv4 or IPv6 literal.
    pub(super) address: String,
    /// The TCP port it listens on.
    pub(super) port: u16,
}

/// Whether a client must authenticate.
///
/// The accounts themselves are not here and never will be: they live in
/// `/var/lib/mica/mqtt-broker-users.toml` on STATE, outside the settings tree
/// and therefore outside everything this API serves.
#[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MqttAuth {
    /// Whether a connecting client must authenticate.
    pub(super) enabled: bool,
}

/// What the `mqtt` reconciler last published about what it did.
///
/// micad's document verbatim when it answers, and `available: false` with a
/// sentence when it does not -- the same posture the network observation
/// takes, for the same reason: an absent observer is not an empty one.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MqttObserved {
    /// Whether micad answered with a live-state document.
    pub(super) available: bool,
    /// The reconciler's own record: the rendered configuration path, the
    /// listener it wrote and the state of the two units.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub(super) state: Option<Value>,
    /// Why there is no document, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<&'static str>,
}

/// The declared MQTT configuration together with what the reconciler did.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MqttOverview {
    /// The stored subtree.
    pub(super) configured: MqttConfiguration,
    /// The reconciler's live state.
    pub(super) observed: MqttObserved,
}

/// Read the MQTT configuration together with the reconciler's live state.
#[utoipa::path(
    get,
    path = V1_MQTT_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The declared `mqtt` subtree and the reconciler's last published state", body = MqttOverview),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The stored subtree is not an `mqtt` document this build can read (`settings_invalid`)", body = ApiError),
        (status = 503, description = "micad is unavailable", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_mqtt_read(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let stored = match state.api.get_settings(MQTT_SETTINGS_PATH).await {
        Ok(value) => value,
        Err(err) => return bus_api_error(&err, Some(MQTT_SETTINGS_PATH)),
    };
    // A subtree this build cannot read is an error and never a default: a
    // console shown `127.0.0.1:1883` for a device bound to `0.0.0.0` would be
    // reporting the wrong device.
    let configured: MqttConfiguration = match serde_json::from_value(stored) {
        Ok(configured) => configured,
        Err(err) => {
            return api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "settings_invalid",
                    format!("the stored MQTT settings could not be read: {err}"),
                )
                .at(MQTT_SETTINGS_PATH),
            );
        }
    };
    let observed = match state.api.get_state(MQTT_SETTINGS_PATH).await {
        Ok(value) => MqttObserved {
            available: true,
            state: Some(value),
            error: None,
        },
        Err(err) => {
            tracing::warn!(error = %err, "live MQTT state unavailable");
            MqttObserved {
                available: false,
                state: None,
                error: Some("the MQTT reconciler has published no state"),
            }
        }
    };
    api_response(
        StatusCode::OK,
        MqttOverview {
            configured,
            observed,
        },
    )
}

/// Replace the MQTT configuration, applied as one reconcile.
///
/// **A `PUT` replaces the whole subtree**: send the document that was read,
/// with the fields that changed. Answers the apply task's id, which is what
/// `GET /api/v1/tasks/{id}` follows -- the broker and the bridge are brought
/// to the new configuration by the `mqtt` reconciler, not by this call.
#[utoipa::path(
    put,
    path = V1_MQTT_PATH,
    context_path = API,
    tag = "resources",
    request_body = MqttConfiguration,
    responses(
        (status = 202, description = "The configuration was written and the reconcile queued; the body carries the task id", body = TaskAccepted),
        (status = 400, description = "The body is not JSON (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 422, description = "The body is not an MQTT document, the listen address is not an IP literal, or the port is 0 (`validation_failed`); or micad rejected the write (`settings_rejected`)", body = ApiError),
        (status = 500, description = "micad failed to write (`settings_io`, `micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_mqtt_write(
    _credential: ApiCredential,
    State(state): State<AppState>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let configured: MqttConfiguration = match json_body(body, Some(MQTT_SETTINGS_PATH)) {
        Ok(configured) => configured,
        Err(response) => return *response,
    };
    // The settings tree takes the address as text and the broker is what finds
    // out it is not one, which on a read-only root means a unit that will not
    // start and a console that reported success. The check is here for the
    // reason the network routes' address check is: the file stays writable
    // without apid, and this is the surface that can still say why.
    if configured
        .listen
        .address
        .parse::<std::net::IpAddr>()
        .is_err()
    {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the listen address is an IPv4 or IPv6 literal, such as `127.0.0.1` for the loopback or `0.0.0.0` for every address".to_string(),
            )
            .at(MQTT_SETTINGS_PATH),
        );
    }
    if configured.listen.port == 0 {
        return api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::apid(
                "validation_failed",
                "the listen port is a TCP port between 1 and 65535; the MQTT default is 1883"
                    .to_string(),
            )
            .at(MQTT_SETTINGS_PATH),
        );
    }
    // Infallible: a struct of scalars.
    let value = match encode(&configured) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    match state.api.set_settings(MQTT_SETTINGS_PATH, &value).await {
        Ok(task_id) => api_response(StatusCode::ACCEPTED, TaskAccepted { task_id }),
        Err(err) => bus_api_error(&err, Some(MQTT_SETTINGS_PATH)),
    }
}
