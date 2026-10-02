//! The custom UI bundles: status, listing, activation, deactivation and removal.

use crate::audit::Source;
use crate::bundle::{CandidateUnavailable, CustomCandidate, Installed, Rejection, Store};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::*;

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UiStatus {
    /// `builtIn` when no custom bundle is active, otherwise `custom`.
    pub(super) mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) custom: Option<CustomUiStatus>,
    /// The newest usable retained generation, or the newest unusable one with
    /// a reason when no generation passes validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) available_custom: Option<AvailableCustomUiStatus>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomUiStatus {
    pub(super) generation: u64,
    pub(super) index_readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) digest_matches: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compatible: Option<bool>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AvailableCustomUiStatus {
    pub(super) generation: u64,
    pub(super) index_readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) digest_matches: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compatible: Option<bool>,
    pub(super) usable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) unavailable_reason: Option<CustomUiUnavailableReason>,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UiBundleList {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) active_generation: Option<u64>,
    pub(super) bundles: Vec<UiBundleDetails>,
    pub(super) retention_limit: usize,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UiBundleDetails {
    pub(super) generation: u64,
    pub(super) index_readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compressed_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expanded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) digest_matches: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) compatible: Option<bool>,
    pub(super) usable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) unavailable_reason: Option<CustomUiUnavailableReason>,
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
pub(crate) struct ActivateUiRequest {
    pub(super) generation: u64,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CustomUiUnavailableReason {
    MissingActivationRecord,
    UnsafeTree,
    IndexUnavailable,
    ManifestInvalid,
    DigestMismatch,
    Incompatible,
}

impl From<CandidateUnavailable> for CustomUiUnavailableReason {
    fn from(reason: CandidateUnavailable) -> Self {
        match reason {
            CandidateUnavailable::MissingActivationRecord => Self::MissingActivationRecord,
            CandidateUnavailable::UnsafeTree => Self::UnsafeTree,
            CandidateUnavailable::IndexUnavailable => Self::IndexUnavailable,
            CandidateUnavailable::ManifestInvalid => Self::ManifestInvalid,
            CandidateUnavailable::DigestMismatch => Self::DigestMismatch,
            CandidateUnavailable::Incompatible => Self::Incompatible,
        }
    }
}

impl From<CustomCandidate> for AvailableCustomUiStatus {
    fn from(candidate: CustomCandidate) -> Self {
        let (name, version) = candidate.manifest.map_or((None, None), |manifest| {
            (Some(manifest.name), Some(manifest.version))
        });
        Self {
            generation: candidate.generation,
            index_readable: candidate.index_readable,
            name,
            version,
            digest_matches: candidate.digest_matches,
            compatible: candidate.compatible,
            usable: candidate.usable,
            unavailable_reason: candidate.unavailable_reason.map(Into::into),
        }
    }
}

impl From<CustomCandidate> for UiBundleDetails {
    fn from(candidate: CustomCandidate) -> Self {
        let (name, version) = candidate.manifest.map_or((None, None), |manifest| {
            (Some(manifest.name), Some(manifest.version))
        });
        Self {
            generation: candidate.generation,
            index_readable: candidate.index_readable,
            name,
            version,
            digest: candidate.digest,
            compressed_bytes: candidate.compressed_bytes,
            expanded_bytes: candidate.expanded_bytes,
            digest_matches: candidate.digest_matches,
            compatible: candidate.compatible,
            usable: candidate.usable,
            unavailable_reason: candidate.unavailable_reason.map(Into::into),
        }
    }
}

pub(super) fn ui_status(store: &Store) -> anyhow::Result<UiStatus> {
    let available_custom = store
        .available_custom(&SERVED_VERSIONS)?
        .map(AvailableCustomUiStatus::from);
    Ok(match store.status()? {
        Installed::BuiltIn => UiStatus {
            mode: "builtIn",
            custom: None,
            available_custom,
        },
        Installed::Custom(ui) => {
            let (name, version) = ui.manifest.map_or((None, None), |manifest| {
                (Some(manifest.name), Some(manifest.version))
            });
            let (digest_matches, compatible) = ui.recorded.map_or((None, None), |recorded| {
                let compatible = match recorded.compat {
                    crate::bundle::CompatCheck::NotRun => None,
                    crate::bundle::CompatCheck::Ran { compatible, .. } => Some(compatible),
                };
                (Some(recorded.digest_matches), compatible)
            });
            UiStatus {
                mode: "custom",
                custom: Some(CustomUiStatus {
                    generation: ui.generation,
                    index_readable: ui.index_readable,
                    name,
                    version,
                    digest_matches,
                    compatible,
                }),
                available_custom,
            }
        }
    })
}

pub(super) async fn load_ui_status(state: &AppState) -> anyhow::Result<UiStatus> {
    let bundles = Arc::clone(&state.bundles);
    match tokio::task::spawn_blocking(move || ui_status(&bundles)).await {
        Ok(status) => status,
        Err(err) => Err(anyhow::Error::new(err)),
    }
}

pub(super) fn ui_bundle_list(store: &Store) -> anyhow::Result<UiBundleList> {
    Ok(UiBundleList {
        active_generation: store.active_generation()?,
        bundles: store
            .candidates(&SERVED_VERSIONS)?
            .into_iter()
            .map(UiBundleDetails::from)
            .collect(),
        retention_limit: 32,
    })
}

pub(super) async fn load_ui_bundles(state: &AppState) -> anyhow::Result<UiBundleList> {
    let bundles = Arc::clone(&state.bundles);
    tokio::task::spawn_blocking(move || ui_bundle_list(&bundles))
        .await
        .map_err(anyhow::Error::new)?
}

/// List every retained custom UI generation.
#[utoipa::path(
    get,
    path = V1_UI_BUNDLES_PATH,
    context_path = API,
    tag = "ui",
    responses(
        (status = 200, description = "Every retained UI version and the active generation", body = UiBundleList),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The bundle store could not be read", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_bundles(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match load_ui_bundles(&state).await {
        Ok(list) => api_response(StatusCode::OK, list),
        Err(err) => ui_status_error(&err),
    }
}

/// Report whether `/` currently selects a custom UI bundle.
#[utoipa::path(
    get,
    path = V1_UI_PATH,
    context_path = API,
    tag = "ui",
    responses(
        (status = 200, description = "The active UI selection and custom bundle health", body = UiStatus),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 500, description = "The bundle store could not be read", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match load_ui_status(&state).await {
        Ok(status) => api_response(StatusCode::OK, status),
        Err(err) => {
            tracing::error!(error = %err, "reading custom UI status failed");
            api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "ui_status_failed",
                    "the custom UI status could not be read".to_string(),
                ),
            )
        }
    }
}

/// Select one exact retained custom bundle after repeating every safety and
/// compatibility check.
#[utoipa::path(
    put,
    path = V1_UI_ACTIVE_PATH,
    context_path = API,
    tag = "ui",
    request_body = ActivateUiRequest,
    responses(
        (status = 200, description = "A validated retained custom UI is now selected", body = UiStatus),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 403, description = "The browser CSRF token is absent or invalid", body = ApiError),
        (status = 409, description = "No retained custom UI passes validation (`custom_ui_unavailable`)", body = ApiError),
        (status = 500, description = "The custom UI pointer could not be updated", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_activate(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Json(request): Json<ActivateUiRequest>,
) -> Response {
    let _selection_guard = state.ui_selection.lock().await;
    let bundles = Arc::clone(&state.bundles);
    let selection = match tokio::task::spawn_blocking(move || {
        bundles.select_generation(request.generation, &SERVED_VERSIONS)
    })
    .await
    {
        Ok(selection) => selection,
        Err(err) => Err(anyhow::Error::new(err)),
    };
    match selection {
        Ok(Some(selection)) => {
            state.audit.record(
                "custom-ui",
                if selection.changed {
                    "activated"
                } else {
                    "no-op"
                },
                &source,
            );
            match load_ui_status(&state).await {
                Ok(status) => api_response(StatusCode::OK, status),
                Err(err) => ui_status_error(&err),
            }
        }
        Ok(None) => api_response(
            StatusCode::CONFLICT,
            ApiError::apid(
                "custom_ui_unavailable",
                "no retained custom UI passes the current safety and API compatibility checks"
                    .to_string(),
            ),
        ),
        Err(err) => {
            tracing::error!(error = %err, "activating retained custom UI failed");
            api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "ui_activation_failed",
                    "the custom UI pointer could not be updated".to_string(),
                ),
            )
        }
    }
}

/// Delete one inactive retained custom UI generation.
#[utoipa::path(
    delete,
    path = V1_UI_BUNDLE_ROUTE,
    context_path = API,
    tag = "ui",
    params(("generation" = u64, Path, description = "The retained generation to delete")),
    responses(
        (status = 204, description = "The inactive retained generation was deleted"),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 403, description = "The browser CSRF token is absent or invalid", body = ApiError),
        (status = 409, description = "The generation is active", body = ApiError),
        (status = 500, description = "The generation could not be deleted", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_delete(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    Path(generation): Path<u64>,
) -> Response {
    let _selection_guard = state.ui_selection.lock().await;
    let bundles = Arc::clone(&state.bundles);
    let deleted = tokio::task::spawn_blocking(move || bundles.delete(generation)).await;
    match deleted {
        Ok(Ok(())) => {
            state.audit.record("custom-ui-delete", "deleted", &source);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(err))
            if matches!(
                err.downcast_ref::<Rejection>(),
                Some(Rejection::DeleteWhileActive(_))
            ) =>
        {
            ui_upload_error(
                StatusCode::CONFLICT,
                "ui_bundle_active",
                "deactivate this UI version before deleting it",
            )
        }
        Ok(Err(err)) => {
            tracing::error!(error = %err, generation, "deleting custom UI failed");
            ui_upload_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ui_delete_failed",
                "the custom UI version could not be deleted",
            )
        }
        Err(err) => {
            tracing::error!(error = %err, generation, "custom UI deletion task did not complete");
            ui_upload_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ui_delete_failed",
                "the custom UI version could not be deleted",
            )
        }
    }
}

/// Deactivate the custom bundle so `/` selects the built-in UI.
#[utoipa::path(
    delete,
    path = V1_UI_ACTIVE_PATH,
    context_path = API,
    tag = "ui",
    responses(
        (status = 200, description = "The built-in UI is now selected", body = UiStatus),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 403, description = "The browser CSRF token is absent or invalid", body = ApiError),
        (status = 500, description = "The custom UI pointer could not be removed", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_deactivate(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
) -> Response {
    let _selection_guard = state.ui_selection.lock().await;
    let bundles = Arc::clone(&state.bundles);
    let deactivated = match tokio::task::spawn_blocking(move || bundles.deactivate()).await {
        Ok(deactivated) => deactivated,
        Err(err) => Err(anyhow::Error::new(err)),
    };
    match deactivated {
        Ok(removed) => {
            state.audit.record(
                "custom-ui",
                if removed { "deactivated" } else { "no-op" },
                &source,
            );
            match load_ui_status(&state).await {
                Ok(status) => api_response(StatusCode::OK, status),
                Err(err) => ui_status_error(&err),
            }
        }
        Err(err) => {
            tracing::error!(error = %err, "deactivating custom UI failed");
            api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::apid(
                    "ui_deactivation_failed",
                    "the custom UI pointer could not be removed".to_string(),
                ),
            )
        }
    }
}

pub(super) fn ui_status_error(err: &anyhow::Error) -> Response {
    tracing::error!(error = %err, "reading custom UI status failed");
    api_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        ApiError::apid(
            "ui_status_failed",
            "the custom UI status could not be read".to_string(),
        ),
    )
}
