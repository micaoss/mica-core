//! Uploading a custom UI bundle.

use crate::audit::Source;
use crate::bundle::Rejection;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::Response;
use futures_util::StreamExt;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use super::*;

/// Stream a `.mica-ui.zip` to DATA, validate and extract it off the async worker,
/// then install it without changing the active generation.
#[utoipa::path(
    post,
    path = V1_UI_BUNDLES_PATH,
    context_path = API,
    tag = "ui",
    request_body(content = String, content_type = "application/zip"),
    responses(
        (status = 201, description = "The package was installed but not activated", body = UiBundleList),
        (status = 401, description = "No API credential was supplied", body = ApiError),
        (status = 403, description = "The browser CSRF token is absent or invalid", body = ApiError),
        (status = 409, description = "The package duplicates, conflicts with, or is incompatible with retained versions", body = ApiError),
        (status = 413, description = "The compressed package exceeds 64 MiB", body = ApiError),
        (status = 415, description = "The body is not application/zip", body = ApiError),
        (status = 422, description = "The ZIP or manifest violates the UI package contract", body = ApiError),
        (status = 507, description = "Writable /mica storage or required headroom is unavailable", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_ui_upload(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    request: Request<Body>,
) -> Response {
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type != Some("application/zip") {
        return ui_upload_refusal(
            &state,
            &source,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "ui_package_type",
            "upload a .mica-ui.zip as application/zip",
        );
    }
    if request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > mica_ui_bundle::MAX_COMPRESSED_BYTES)
    {
        return ui_upload_refusal(
            &state,
            &source,
            StatusCode::PAYLOAD_TOO_LARGE,
            "ui_package_too_large",
            "the compressed package limit is 64 MiB",
        );
    }

    let upload_dir = state.bundles.root().join("staging");
    match tokio::fs::create_dir(&upload_dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let real_directory = tokio::fs::symlink_metadata(&upload_dir)
                .await
                .is_ok_and(|metadata| metadata.file_type().is_dir());
            if !real_directory {
                tracing::error!("UI upload staging path is not a real directory");
                return ui_upload_refusal(
                    &state,
                    &source,
                    StatusCode::INSUFFICIENT_STORAGE,
                    "ui_storage_unavailable",
                    "writable /mica UI storage is unavailable",
                );
            }
        }
        Err(err) => {
            tracing::error!(error = %err, "creating UI upload staging directory failed");
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INSUFFICIENT_STORAGE,
                "ui_storage_unavailable",
                "writable /mica UI storage is unavailable",
            );
        }
    }
    if let Err(err) =
        tokio::fs::set_permissions(&upload_dir, std::fs::Permissions::from_mode(0o700)).await
    {
        tracing::error!(error = %err, "protecting UI upload staging directory failed");
        return ui_upload_refusal(
            &state,
            &source,
            StatusCode::INSUFFICIENT_STORAGE,
            "ui_storage_unavailable",
            "writable /mica UI storage is unavailable",
        );
    }
    let upload = upload_dir.join(format!(
        "{:032x}",
        u128::from_ne_bytes(micad_settings::random_bytes())
    ));
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&upload)
        .await
    {
        Ok(file) => file,
        Err(err) => {
            tracing::error!(error = %err, "creating UI upload file failed");
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INSUFFICIENT_STORAGE,
                "ui_storage_unavailable",
                "the upload staging file could not be created",
            );
        }
    };
    let mut stream = request.into_body().into_data_stream();
    let mut compressed = 0_u64;
    use tokio::io::AsyncWriteExt as _;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                let _ = tokio::fs::remove_file(&upload).await;
                tracing::warn!(error = %err, "UI package upload was interrupted");
                return ui_upload_refusal(
                    &state,
                    &source,
                    StatusCode::BAD_REQUEST,
                    "ui_upload_interrupted",
                    "the upload was interrupted",
                );
            }
        };
        compressed = compressed.saturating_add(chunk.len() as u64);
        if compressed > mica_ui_bundle::MAX_COMPRESSED_BYTES {
            let _ = tokio::fs::remove_file(&upload).await;
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::PAYLOAD_TOO_LARGE,
                "ui_package_too_large",
                "the compressed package limit is 64 MiB",
            );
        }
        if let Err(err) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&upload).await;
            tracing::error!(error = %err, "writing UI upload failed");
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INSUFFICIENT_STORAGE,
                "ui_storage_unavailable",
                "the package could not be written to /mica",
            );
        }
    }
    if let Err(err) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&upload).await;
        tracing::error!(error = %err, "syncing UI upload failed");
        return ui_upload_refusal(
            &state,
            &source,
            StatusCode::INSUFFICIENT_STORAGE,
            "ui_storage_unavailable",
            "the package could not be persisted to /mica",
        );
    }
    drop(file);

    let inspect_path = upload.clone();
    let info =
        match tokio::task::spawn_blocking(move || mica_ui_bundle::inspect(&inspect_path)).await {
            Ok(Ok(info)) => info,
            Ok(Err(err)) => {
                let _ = tokio::fs::remove_file(&upload).await;
                tracing::warn!(error = %err, "inspecting UI package failed");
                return ui_upload_refusal(
                    &state,
                    &source,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ui_package_invalid",
                    "the ZIP or manifest violates the UI package contract",
                );
            }
            Err(err) => {
                let _ = tokio::fs::remove_file(&upload).await;
                tracing::error!(error = %err, "UI package validator did not complete");
                return ui_upload_refusal(
                    &state,
                    &source,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ui_package_failed",
                    "the package validator did not complete",
                );
            }
        };

    let root = state.bundles.root().to_path_buf();
    let required = info.expanded_bytes.saturating_add(state.data_reserve);
    let free = tokio::task::spawn_blocking(move || {
        let stat = rustix::fs::statvfs(&root)?;
        Ok::<u64, std::io::Error>(stat.f_bavail.saturating_mul(stat.f_frsize))
    })
    .await;
    match free {
        Ok(Ok(free)) if free >= required => {}
        Ok(Ok(_)) => {
            let _ = tokio::fs::remove_file(&upload).await;
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INSUFFICIENT_STORAGE,
                "ui_storage_headroom",
                &format!(
                    "upload refused because extraction would leave less than {} MiB free",
                    state.data_reserve / MIB
                ),
            );
        }
        Ok(Err(err)) => {
            let _ = tokio::fs::remove_file(&upload).await;
            tracing::error!(error = %err, "reading UI storage capacity failed");
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INSUFFICIENT_STORAGE,
                "ui_storage_unavailable",
                "free space under /mica could not be measured",
            );
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&upload).await;
            tracing::error!(error = %err, "UI storage capacity check did not complete");
            return ui_upload_refusal(
                &state,
                &source,
                StatusCode::INTERNAL_SERVER_ERROR,
                "ui_package_failed",
                "the storage capacity check did not complete",
            );
        }
    }

    let _selection_guard = state.ui_selection.lock().await;
    let store = Arc::clone(&state.bundles);
    let upload_for_install = upload.clone();
    let package_sizes = (info.compressed_bytes, info.expanded_bytes);
    let installed = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
        let generation = store.next_generation()?;
        let staging = store.staging_dir(generation);
        let extraction = mica_ui_bundle::extract(&upload_for_install, &staging);
        if let Err(err) = extraction {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        if let Err(err) = store.install(
            generation,
            &SERVED_VERSIONS,
            package_sizes.0,
            package_sizes.1,
        ) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        Ok(generation)
    })
    .await;
    let _ = tokio::fs::remove_file(&upload).await;
    match installed {
        Ok(Ok(_generation)) => {
            state.audit.record("custom-ui-upload", "installed", &source);
            match load_ui_bundles(&state).await {
                Ok(list) => api_response(StatusCode::CREATED, list),
                Err(err) => ui_status_error(&err),
            }
        }
        Ok(Err(err)) => {
            let (status, code, message) = match err.downcast_ref::<Rejection>() {
                Some(
                    Rejection::DuplicatePackage(_)
                    | Rejection::NameVersionConflict { .. }
                    | Rejection::RetentionLimit(_)
                    | Rejection::Incompatible { .. },
                ) => (
                    StatusCode::CONFLICT,
                    "ui_package_conflict",
                    "the package duplicates, conflicts with, or is incompatible with retained versions",
                ),
                Some(_) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ui_package_rejected",
                    "the package tree or manifest violates the UI package contract",
                ),
                None => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ui_package_failed",
                    "the package could not be installed",
                ),
            };
            tracing::warn!(error = %err, "installing UI package failed");
            ui_upload_refusal(&state, &source, status, code, message)
        }
        Err(err) => {
            tracing::error!(error = %err, "UI package installer did not complete");
            ui_upload_refusal(
                &state,
                &source,
                StatusCode::INTERNAL_SERVER_ERROR,
                "ui_package_failed",
                "the package installer did not complete",
            )
        }
    }
}

pub(super) fn ui_upload_refusal(
    state: &AppState,
    source: &str,
    status: StatusCode,
    code: &'static str,
    message: &str,
) -> Response {
    state.audit.record("custom-ui-upload", "refused", source);
    ui_upload_error(status, code, message)
}

pub(super) fn ui_upload_error(status: StatusCode, code: &'static str, message: &str) -> Response {
    api_response(status, ApiError::apid(code, message.to_string()))
}
