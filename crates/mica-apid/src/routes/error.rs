//! The API error envelope and the mapping of micad and task errors onto it.

use crate::assets::mime::CacheClass;
use crate::redact;
use crate::settings_api::InvalidTaskPayload;
use axum::Json;
use axum::http::header::{CACHE_CONTROL, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::*;

/// Every `/api/` response is JSON and is never cacheable.
pub(crate) fn api_response(status: StatusCode, body: impl serde::Serialize) -> Response {
    (
        status,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
        Json(body),
    )
        .into_response()
}

/// The one shape every failure under `/api/` takes.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ApiError {
    pub(super) error: ApiErrorDetail,
}

/// The API error envelope payload.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(crate) struct ApiErrorDetail {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) path: Option<String>,
}

impl ApiError {
    pub(crate) fn apid(code: &'static str, message: String) -> Self {
        Self::new(code, message, "apid")
    }

    pub(crate) fn micad(code: &'static str, message: String) -> Self {
        Self::new(code, message, "micad")
    }

    pub(super) fn new(code: &'static str, message: String, source: &'static str) -> Self {
        Self {
            error: ApiErrorDetail {
                code,
                message,
                source,
                path: None,
            },
        }
    }

    pub(super) fn at(mut self, path: &str) -> Self {
        self.error.path = Some(path.to_string());
        self
    }
}

/// The envelope for the condition every API collection item route shares: a
/// well-formed identifier that names no item.
///
/// One shared function and not one per handler, which is what the rule
/// requires: a collection route added later inherits the 404 by reaching for
/// this, rather than by remembering a
/// decision, and the 422 beside it stays reserved for an identifier that is not
/// well formed at all.
///
/// The HTML panes answer **422** for the same condition, deliberately and on
/// the record. Its paired test is
/// `the_builtin_revoke_pane_answers_422_where_the_api_answers_404`.
pub(super) fn item_not_found(collection: &str, identifier: &str) -> Response {
    api_response(
        StatusCode::NOT_FOUND,
        ApiError::apid(
            "settings_not_found",
            format!("no item of `{collection}` is identified by `{identifier}`"),
        )
        .at(collection),
    )
}

/// 204 with `no-store`, the answer every write in this cluster gives.
pub(super) fn no_content() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(CACHE_CONTROL, CacheClass::NoStore.header_value())],
    )
        .into_response()
}

/// Read a JSON body, or the 400 that says it was not JSON at all.
///
/// The dot-path is an `Option` because the `path` member is: every route of
/// the network cluster writes one subtree and names it, but `POST /api/v1/setup`
/// writes three and a malformed body there is not about any one of them. A
/// member present with a meaningless value is worse than an absent one, which
/// is the rule [`ApiErrorDetail::path`] already states.
pub(super) fn json_body<T: serde::de::DeserializeOwned>(
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
    path: Option<&str>,
) -> Result<T, Box<Response>> {
    let at = |error: ApiError| match path {
        Some(path) => error.at(path),
        None => error,
    };
    let Json(value) = body.map_err(|rejection| {
        Box::new(api_response(
            StatusCode::BAD_REQUEST,
            at(ApiError::apid("request_invalid", rejection.body_text())),
        ))
    })?;
    serde_json::from_value(value).map_err(|err| {
        Box::new(api_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            at(ApiError::apid("validation_failed", err.to_string())),
        ))
    })
}

/// One answer shape for both roots: the value redacted, or the error envelope
/// classified from what micad said.
pub(super) fn resource_response(value: anyhow::Result<Value>, path: &str) -> Response {
    match value {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, path))),
        Err(err) => bus_api_error(&err, Some(path)),
    }
}

/// The table, applied to a failed micad call.
///
/// The classification is translated and the message is not. micad maps its
/// `SettingsError` onto five error names — two interface-scoped, three fdo —
/// and zbus carries the name back, so the distinction exists all the way to
/// here and only apid can lose it; the message is micad's own words because no
/// phrasing apid could pre-write would say which field was wrong.
///
/// The concrete `zbus::Error` is recovered by downcast: `bus_client.rs`
/// converts with `err.into()`, and that conversion stores the error rather
/// than flattening it, so the name is readable here.
///
/// `path` is an `Option` because the error envelope makes the member optional — *"present
/// only when the failure names a dot-path"* — and the actions name none: a
/// power verb and a transient root password write no setting at all, so there
/// is no dot-path at fault to report. Every route that does name one passes
/// `Some`, and the classification above is shared rather than copied.
pub(crate) fn bus_api_error(err: &anyhow::Error, path: Option<&str>) -> Response {
    tracing::warn!(error = %err, path = path.unwrap_or_default(), "micad call failed");
    let (status, error) = if err.downcast_ref::<InvalidTaskPayload>().is_some() {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::micad("micad_failed", format!("{err:#}")),
        )
    } else if err
        .downcast_ref::<crate::bus_client::MicadCallTimeout>()
        .is_some()
    {
        (
            StatusCode::GATEWAY_TIMEOUT,
            ApiError::apid("micad_timeout", format!("{err:#}")),
        )
    } else {
        match err.downcast_ref::<zbus::Error>() {
            Some(zbus::Error::MethodError(name, message, _)) => {
                // An fdo error with no message is still a classification; the name
                // is the most specific thing left to say.
                let message = message.clone().unwrap_or_else(|| name.to_string());
                match name.as_str() {
                    MICAD_NOT_FOUND => (
                        StatusCode::NOT_FOUND,
                        ApiError::micad("settings_not_found", message),
                    ),
                    MICAD_READ_ONLY => (
                        StatusCode::CONFLICT,
                        ApiError::micad("settings_read_only", message),
                    ),
                    FDO_INVALID_ARGS => (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        ApiError::micad("settings_rejected", message),
                    ),
                    FDO_IO_ERROR => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ApiError::micad("settings_io", message),
                    ),
                    FDO_FAILED => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        ApiError::micad("micad_failed", message),
                    ),
                    _ => micad_unreachable(err),
                }
            }
            _ => micad_unreachable(err),
        }
    };
    // Omitted rather than nulled or emptied when there is none: the member
    // carries `skip_serializing_if`, so an action's envelope simply has no
    // `path` key.
    let error = match path {
        Some(path) => error.at(path),
        None => error,
    };
    let mut response = api_response(status, error);
    // `Retry-After` belongs to exactly one class, and 503 is that class:
    // apid is up and answering, and the proxy cache is dropped after a failed
    // call so the next request reconnects.
    if status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static(RETRY_AFTER_SECONDS));
    }
    response
}

/// Task lookup has the same transport classifications as other micad calls,
/// but its missing item is not a missing settings path.
pub(super) fn task_api_error(err: &anyhow::Error, id: &str) -> Response {
    if is_task_not_found(err) {
        return api_response(
            StatusCode::NOT_FOUND,
            ApiError::micad("task_not_found", format!("task not found: `{id}`")),
        );
    }
    bus_api_error(err, None)
}

pub(super) fn is_task_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<crate::settings_api::TaskNotFound>()
        .is_some()
        || matches!(
            err.downcast_ref::<zbus::Error>(),
            Some(zbus::Error::MethodError(name, _, _)) if name.as_str() == MICAD_NOT_FOUND
        )
}

/// The last row, which is exhaustive over everything the three above do not
/// name: the call could not be made at all. `source` is apid because this is a
/// statement about this server rather than about the request.
pub(super) fn micad_unreachable(err: &anyhow::Error) -> (StatusCode, ApiError) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        ApiError::apid("micad_unreachable", format!("{err:#}")),
    )
}

/// A typed value as the JSON micad stores. The settings types serialize by
/// construction; were one ever not to, the request is answered 500 and the
/// daemon keeps running.
pub(crate) fn encode<T: serde::Serialize>(value: T) -> Result<Value, Box<Response>> {
    serde_json::to_value(value).map_err(|err| {
        tracing::error!(error = %err, "encoding a settings value failed");
        Box::new(api_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::apid(
                "encoding_failed",
                format!("the value could not be encoded: {err}"),
            ),
        ))
    })
}
