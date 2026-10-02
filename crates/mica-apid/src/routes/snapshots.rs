//! Diagnostic snapshots.

use crate::assets::mime::CacheClass;
use crate::audit::Source;
use crate::diagnostics::{self, Collector, SnapshotSummary};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::sync::Arc;

use super::*;

/// The retention and schema facts a client needs to read the collection.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotRetention {
    /// The most snapshots kept; publishing one more removes the oldest.
    pub(super) max_snapshots: usize,
    /// The most bytes kept across all snapshots.
    pub(super) max_total_bytes: u64,
    /// The most bytes one snapshot may be; larger is refused.
    pub(super) max_snapshot_bytes: usize,
    /// The snapshot schema version this build produces.
    pub(super) schema_version: u64,
    /// The redaction schema version this build applies.
    pub(super) redaction_schema_version: u64,
}

/// The snapshot collection.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotList {
    /// Every stored snapshot, oldest first.
    pub(super) snapshots: Vec<SnapshotSummary>,
    /// The bounds the store enforces.
    pub(super) retention: SnapshotRetention,
}

/// The answer to a collection: the stored snapshot and how collecting went.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotCollected {
    /// The snapshot as the list describes it.
    pub(super) snapshot: SnapshotSummary,
    /// Wall time the collection took.
    pub(super) elapsed_millis: u64,
    /// Per source, `ok`, `unavailable` or `timeout`.
    pub(super) sections: std::collections::BTreeMap<String, String>,
    /// Fields the redaction schema did not name and therefore dropped.
    pub(super) dropped_fields: usize,
    /// Fields and strings the redaction pass replaced.
    pub(super) redacted_fields: usize,
}

pub(super) fn snapshot_retention() -> SnapshotRetention {
    SnapshotRetention {
        max_snapshots: diagnostics::MAX_SNAPSHOTS,
        max_total_bytes: diagnostics::MAX_TOTAL_BYTES,
        max_snapshot_bytes: diagnostics::MAX_SNAPSHOT_BYTES,
        schema_version: diagnostics::SCHEMA_VERSION,
        redaction_schema_version: diagnostics::REDACTION_SCHEMA_VERSION,
    }
}

/// A store failure, as the error envelope: apid's own, 500.
pub(super) fn diagnostics_io_error(err: &anyhow::Error, doing: &str) -> Response {
    tracing::error!(error = %err, "diagnostics store: {doing} failed");
    api_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        ApiError::apid("diagnostics_io", format!("{doing} failed: {err:#}")),
    )
}

pub(super) fn snapshot_not_found(id: &str) -> Response {
    api_response(
        StatusCode::NOT_FOUND,
        ApiError::apid(
            "snapshot_not_found",
            format!("no diagnostic snapshot `{id}`"),
        ),
    )
}

/// List the stored diagnostic snapshots.
///
/// Oldest first, each with its id, size, `collectedAt`, schema version and
/// machine id, plus the retention bounds the store enforces.
#[utoipa::path(
    get,
    path = V1_DIAGNOSTICS_SNAPSHOTS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The stored snapshots and the retention bounds", body = SnapshotList),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "The store could not be read (`diagnostics_io`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_diagnostics_list(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let store = Arc::clone(&state.diagnostics);
    match tokio::task::spawn_blocking(move || store.list()).await {
        Ok(Ok(snapshots)) => api_response(
            StatusCode::OK,
            SnapshotList {
                snapshots,
                retention: snapshot_retention(),
            },
        ),
        Ok(Err(err)) => diagnostics_io_error(&err, "listing snapshots"),
        Err(err) => diagnostics_io_error(&anyhow::anyhow!(err), "listing snapshots"),
    }
}

/// Collect a diagnostic snapshot.
///
/// Reads every micad surface the schema names — system information,
/// telemetry, failure evidence, storage, time, observed network, live
/// state — under a per-section timeout inside one deadline, assembles the
/// versioned snapshot, redacts it against the allowlist schema (a field the
/// schema does not name does not ship), and publishes it whole to the store
/// or not at all. A source that does not answer is an absent member with
/// the reason; the snapshot is produced regardless. Bounded on disk: the
/// oldest snapshots are removed to stay under the retention caps.
///
/// Answers **201** with the stored snapshot's summary and the collection
/// report. **409** while another collection is running.
#[utoipa::path(
    post,
    path = V1_DIAGNOSTICS_SNAPSHOTS_PATH,
    context_path = API,
    tag = "actions",
    responses(
        (status = 201, description = "The snapshot was collected, redacted and published", body = SnapshotCollected),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 409, description = "A collection is already running (`diagnostics_busy`)", body = ApiError),
        (status = 500, description = "The snapshot could not be published (`diagnostics_io`): the store is unwritable or the snapshot is above the size cap; nothing was written", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_diagnostics_collect(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    let Ok(_guard) = state.collecting.try_lock() else {
        return api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "diagnostics_busy",
                "a snapshot is being collected; retry when it has been published".to_string(),
            ),
        );
    };
    let collected = Collector::new(state.api.as_ref()).collect().await;
    let store = Arc::clone(&state.diagnostics);
    let snapshot = collected.snapshot;
    let report = collected.report;
    let published = tokio::task::spawn_blocking(move || store.publish(&snapshot)).await;
    match published {
        Ok(Ok(summary)) => {
            state
                .audit
                .record("diagnostics-snapshot", "collected", &source);
            api_response(
                StatusCode::CREATED,
                SnapshotCollected {
                    snapshot: summary,
                    elapsed_millis: u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
                    sections: report
                        .sections
                        .iter()
                        .map(|(name, status)| ((*name).to_string(), status.as_str().to_string()))
                        .collect(),
                    dropped_fields: report.redaction.dropped_fields,
                    redacted_fields: report.redaction.redacted_fields,
                },
            )
        }
        Ok(Err(err)) => {
            state
                .audit
                .record("diagnostics-snapshot", "refused", &source);
            diagnostics_io_error(&err, "publishing the snapshot")
        }
        Err(err) => diagnostics_io_error(&anyhow::anyhow!(err), "publishing the snapshot"),
    }
}

/// Export one diagnostic snapshot.
///
/// The stored bytes, verbatim and already redacted, as an attachment named
/// `mica-diagnostics-<machine id prefix>-<id>.json` so a browser saves it
/// under a name support can file. Never cached.
#[utoipa::path(
    get,
    path = V1_DIAGNOSTICS_SNAPSHOT_ROUTE,
    context_path = API,
    tag = "resources",
    params(("id" = String, Path, description = "The snapshot id from the collection listing")),
    responses(
        (status = 200, description = "The snapshot document (`schemaVersion` names its shape); `Content-Disposition: attachment`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "No such snapshot (`snapshot_not_found`)", body = ApiError),
        (status = 500, description = "The store could not be read (`diagnostics_io`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_diagnostics_snapshot(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(snapshot_id) = id.parse::<u64>() else {
        return snapshot_not_found(&id);
    };
    let store = Arc::clone(&state.diagnostics);
    match tokio::task::spawn_blocking(move || store.read(snapshot_id)).await {
        Ok(Ok(Some(bytes))) => {
            let machine: String = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/system/machineId/id")
                        .and_then(Value::as_str)
                        .map(|id| id.chars().take(8).collect())
                })
                .unwrap_or_else(|| "unknown".to_string());
            let disposition =
                format!("attachment; filename=\"mica-diagnostics-{machine}-{snapshot_id}.json\"");
            (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, "application/json"),
                    (CACHE_CONTROL, CacheClass::NoStore.header_value()),
                    (CONTENT_DISPOSITION, disposition.as_str()),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(Ok(None)) => snapshot_not_found(&id),
        Ok(Err(err)) => diagnostics_io_error(&err, "reading the snapshot"),
        Err(err) => diagnostics_io_error(&anyhow::anyhow!(err), "reading the snapshot"),
    }
}

/// Delete one diagnostic snapshot.
///
/// The explicit retention operation: the store also removes the oldest to
/// stay under its caps, and this is how an operator removes one sooner.
/// Answers **204**; **404** when there is no such snapshot.
#[utoipa::path(
    delete,
    path = V1_DIAGNOSTICS_SNAPSHOT_ROUTE,
    context_path = API,
    tag = "actions",
    params(("id" = String, Path, description = "The snapshot id from the collection listing")),
    responses(
        (status = 204, description = "The snapshot was removed"),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "No such snapshot (`snapshot_not_found`)", body = ApiError),
        (status = 500, description = "The store could not be written (`diagnostics_io`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_diagnostics_delete(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Path(id): Path<String>,
) -> Response {
    let Ok(snapshot_id) = id.parse::<u64>() else {
        return snapshot_not_found(&id);
    };
    let store = Arc::clone(&state.diagnostics);
    match tokio::task::spawn_blocking(move || store.delete(snapshot_id)).await {
        Ok(Ok(true)) => {
            state
                .audit
                .record("diagnostics-snapshot", "deleted", &source);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Ok(false)) => snapshot_not_found(&id),
        Ok(Err(err)) => diagnostics_io_error(&err, "deleting the snapshot"),
        Err(err) => diagnostics_io_error(&anyhow::anyhow!(err), "deleting the snapshot"),
    }
}
