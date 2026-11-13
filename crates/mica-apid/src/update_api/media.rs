//! Update archives on external media: listing the ones on a mounted volume,
//! and importing one through the same staging and micad import as a browser
//! upload.
//!
//! micad mounts a volume read-only under the media root; apid only reads
//! there. A volume is listed from its root and its `mica-updates/` directory,
//! not recursively, and only regular `*.micaupd` files count: directories are
//! opened and files read through directory descriptors that never follow a
//! symbolic link. Nothing a file name says is trusted: the archive is
//! checked by `mica-deploy import` like any other.

use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::audit::Source;
use crate::routes::{API, ApiCredential, ApiError, AppState, api_response};

use super::{MAX_ARCHIVE_BYTES, Staging, body_rejection};

pub(crate) const V1_UPDATE_MEDIA_PATH: &str = "/v1/update/media";
pub(crate) const V1_UPDATE_MEDIA_IMPORT_PATH: &str = "/v1/update/media/import";

/// The most archives listed per volume.
const MAX_CANDIDATES: usize = 100;

/// The one directory below a volume's root that is listed too.
const UPDATES_DIR: &str = "mica-updates";

/// An update archive on a volume, as listed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaCandidate {
    /// Opaque: the volume, the file and its size and change time. A file
    /// replaced or changed since the listing gets another id.
    id: String,
    /// The volume's `id` from `GET /api/v1/storage/media`.
    media_id: String,
    /// The file's path on the volume, for display: `name.micaupd` or
    /// `mica-updates/name.micaupd`.
    name: String,
    size_bytes: u64,
    /// False for a file larger than an upload may be (`reason`
    /// `archive_too_large`).
    importable: bool,
    reason: Option<&'static str>,
}

/// `GET /api/v1/update/media`.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaCandidates {
    items: Vec<MediaCandidate>,
    /// Volumes with more archives than are listed.
    truncated: Vec<String>,
    /// Volumes that could not be read.
    unreadable: Vec<String>,
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct MediaQuery {
    /// Only this volume.
    media_id: Option<String>,
}

/// `POST /api/v1/update/media/import`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MediaImportRequest {
    media_id: String,
    candidate_id: String,
}

/// A volume id as micad makes them: 20 lowercase hexadecimal characters.
fn valid_media_id(id: &str) -> bool {
    id.len() == 20
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn open_dir(at: impl AsFd, name: impl rustix::path::Arg) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat(
        at,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

/// One archive found on a volume.
struct Found {
    candidate: MediaCandidate,
    /// Under [`UPDATES_DIR`] rather than the root.
    nested: bool,
    file_name: String,
}

fn candidate_id(media_id: &str, name: &str, stat: &rustix::fs::Stat) -> String {
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    for part in [
        media_id.as_bytes(),
        name.as_bytes(),
        &stat.st_size.to_le_bytes(),
        &stat.st_ino.to_le_bytes(),
        &stat.st_mtime.to_le_bytes(),
        &stat.st_mtime_nsec.to_le_bytes(),
    ] {
        context.update(part);
        context.update(&[0]);
    }
    hex::encode(&context.finish().as_ref()[..10])
}

/// The archives on the volume `media_id` under `root`, in name order, and
/// whether there were more than are listed.
fn scan(root: &Path, media_id: &str) -> rustix::io::Result<(Vec<Found>, bool)> {
    let volume = open_dir(CWD, root.join(media_id))?;
    let mut found = Vec::new();
    for nested in [false, true] {
        let dir = if nested {
            match open_dir(&volume, UPDATES_DIR) {
                Ok(dir) => dir,
                Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => continue,
                Err(err) => return Err(err),
            }
        } else {
            open_dir(&volume, ".")?
        };
        let mut names: Vec<String> = rustix::fs::Dir::read_from(&dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str().ok().map(str::to_string))
            .filter(|name| name.ends_with(".micaupd") && !name.starts_with('.'))
            .collect();
        names.sort();
        for file_name in names {
            let Ok(stat) = rustix::fs::statat(&dir, file_name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                continue;
            }
            if found.len() == MAX_CANDIDATES {
                return Ok((found, true));
            }
            let name = if nested {
                format!("{UPDATES_DIR}/{file_name}")
            } else {
                file_name.clone()
            };
            let size_bytes = u64::try_from(stat.st_size).unwrap_or(0);
            let importable = size_bytes <= MAX_ARCHIVE_BYTES;
            found.push(Found {
                candidate: MediaCandidate {
                    id: candidate_id(media_id, &name, &stat),
                    media_id: media_id.to_string(),
                    name,
                    size_bytes,
                    importable,
                    reason: (!importable).then_some("archive_too_large"),
                },
                nested,
                file_name,
            });
        }
    }
    Ok((found, false))
}

/// The volume ids under `root`, in order.
fn volumes(root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|id| valid_media_id(id))
        .collect();
    ids.sort();
    ids
}

fn list(root: &Path, only: Option<&str>) -> MediaCandidates {
    let mut out = MediaCandidates {
        items: Vec::new(),
        truncated: Vec::new(),
        unreadable: Vec::new(),
    };
    let ids = only.map_or_else(|| volumes(root), |id| vec![id.to_string()]);
    for id in ids {
        match scan(root, &id) {
            Ok((found, more)) => {
                out.items.extend(found.into_iter().map(|f| f.candidate));
                if more {
                    out.truncated.push(id);
                }
            }
            Err(err) => {
                tracing::warn!(media = %id, error = %err, "listing update archives on a volume failed");
                out.unreadable.push(id);
            }
        }
    }
    out
}

/// Why an import from a volume was refused before anything was staged.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// The volume is not mounted now.
    Removed,
    /// No archive with this id: replaced, changed or gone since the listing.
    Changed,
    TooLarge,
}

/// The archive `wanted` on the volume `media_id`, opened, and its size.
fn open_candidate(
    root: &Path,
    media_id: &str,
    wanted: &str,
) -> Result<(std::fs::File, u64), Refusal> {
    let (found, _) = scan(root, media_id).map_err(|_| Refusal::Removed)?;
    let found = found
        .into_iter()
        .find(|f| f.candidate.id == wanted)
        .ok_or(Refusal::Changed)?;
    if !found.candidate.importable {
        return Err(Refusal::TooLarge);
    }
    let volume = open_dir(CWD, root.join(media_id)).map_err(|_| Refusal::Removed)?;
    let dir = if found.nested {
        open_dir(&volume, UPDATES_DIR).map_err(|_| Refusal::Changed)?
    } else {
        volume
    };
    let file = rustix::fs::openat(
        &dir,
        found.file_name.as_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| Refusal::Changed)?;
    // What was opened is what was listed: the same file, the same size and
    // change time, or the id it was listed under would differ.
    let stat = rustix::fs::fstat(&file).map_err(|_| Refusal::Removed)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || candidate_id(media_id, &found.candidate.name, &stat) != wanted
    {
        return Err(Refusal::Changed);
    }
    Ok((std::fs::File::from(file), found.candidate.size_bytes))
}

fn refuse(status: StatusCode, code: &'static str, message: &str) -> Response {
    api_response(status, ApiError::apid(code, message.to_string()))
}

fn removed() -> Response {
    refuse(
        StatusCode::NOT_FOUND,
        "media_removed",
        "the volume is not mounted now: unplugged, unmounted or ejected",
    )
}

fn changed() -> Response {
    refuse(
        StatusCode::CONFLICT,
        "media_candidate_changed",
        "the archive is not on the volume as it was listed; list the volume again",
    )
}

/// List the update archives on external media.
///
/// Reads the volumes micad mounted read-only (`GET /api/v1/storage/media`,
/// `state` `mounted_readonly`): each volume's root and its `mica-updates/`
/// directory, not recursively, regular `*.micaupd` files only, never through
/// a symbolic link, at most 100 per volume in name order (`truncated` names
/// the volumes with more). Only names and sizes are read; nothing is opened,
/// verified or installed. `name` is the file's, for display only: what an
/// archive is, is known once it is imported. A file larger than an upload may
/// be is listed with `importable` false. Without `mediaId`, every mounted
/// volume is listed.
#[utoipa::path(
    get,
    path = V1_UPDATE_MEDIA_PATH,
    context_path = API,
    tag = "update",
    params(MediaQuery),
    responses(
        (status = 200, description = "The archives found, with the volumes listed only in part (`truncated`) or not at all (`unreadable`)", body = MediaCandidates),
        (status = 400, description = "`mediaId` is not a volume id (`request_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 404, description = "The volume is not mounted now (`media_removed`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_media(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Query(query): Query<MediaQuery>,
) -> Response {
    if let Some(id) = &query.media_id {
        if !valid_media_id(id) {
            return refuse(
                StatusCode::BAD_REQUEST,
                "request_invalid",
                "mediaId is a volume id from GET /api/v1/storage/media",
            );
        }
        if !state.media_root.join(id).is_dir() {
            return removed();
        }
    }
    let root = state.media_root.clone();
    match tokio::task::spawn_blocking(move || list(&root, query.media_id.as_deref())).await {
        Ok(listed) => api_response(StatusCode::OK, listed),
        Err(err) => {
            tracing::error!(error = %err, "listing update archives on media failed");
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "media_unreadable",
                "the media could not be listed",
            )
        }
    }
}

/// Import an update archive from external media.
///
/// Copies the archive `candidateId` from the volume `mediaId` into the upload
/// directory, then hands it to micad exactly as a browser upload is
/// (`POST /api/v1/update/import`): the same size limit and magic check, the
/// same `mica-deploy import`, every signature, product and anti-rollback
/// check on the copied bytes. The file is opened through the volume's
/// directory without following a link and must still be the file listed, by
/// size and change time; a file replaced or changed since is
/// `media_candidate_changed`, a volume gone before or during the copy is
/// `media_removed`, and the partial copy is removed. While the copy runs the
/// file is open, so the volume cannot be unmounted or ejected. Nothing is
/// installed: the install is the separate `POST /api/v1/update/install`.
///
/// Answers **202**: poll `GET /api/v1/update` for the import's outcome.
#[utoipa::path(
    post,
    path = V1_UPDATE_MEDIA_IMPORT_PATH,
    context_path = API,
    tag = "actions",
    request_body = MediaImportRequest,
    responses(
        (status = 202, description = "The archive was copied and the import started; poll the update state"),
        (status = 400, description = "Invalid JSON (`request_invalid`), or the file does not begin with the archive magic (`archive_invalid`)", body = ApiError),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 403, description = "A browser session mutation omitted or supplied the wrong CSRF token (`csrf_invalid`)", body = ApiError),
        (status = 404, description = "The volume is not mounted now, or went away during the copy (`media_removed`)", body = ApiError),
        (status = 409, description = "The archive is not on the volume as listed (`media_candidate_changed`), or another update operation is running (`policy_refused`)", body = ApiError),
        (status = 413, description = "The archive is larger than this device accepts (`archive_too_large`)", body = ApiError),
        (status = 500, description = "The copy could not be staged for another reason (`update_staging_failed`)", body = ApiError),
        (status = 503, description = "DATA is not there or is read-only (`update_storage_unavailable`), or micad could not be called (`micad_unreachable`)", body = ApiError),
        (status = 507, description = "The copy does not fit on DATA (`update_storage_full`)", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_update_media_import(
    _credential: ApiCredential,
    State(state): State<AppState>,
    Source(source): Source,
    body: Result<Json<MediaImportRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    use tokio::io::AsyncReadExt as _;

    let Json(request) = match body {
        Ok(body) => body,
        Err(rejection) => return body_rejection(rejection),
    };
    if !valid_media_id(&request.media_id) {
        return removed();
    }
    let root = state.media_root.clone();
    let (media_id, candidate_id) = (request.media_id.clone(), request.candidate_id.clone());
    let opened =
        tokio::task::spawn_blocking(move || open_candidate(&root, &media_id, &candidate_id)).await;
    let (file, size) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(Refusal::Removed)) => return removed(),
        Ok(Err(Refusal::Changed)) => return changed(),
        Ok(Err(Refusal::TooLarge)) => {
            return refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                "archive_too_large",
                "this device accepts an update archive of at most 2 GiB",
            );
        }
        Err(err) => {
            tracing::error!(error = %err, "opening an update archive on media failed");
            return removed();
        }
    };
    let mut staging = match Staging::create(&state).await {
        Ok(staging) => staging,
        Err(response) => return *response,
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0_u8; 1 << 20];
    let mut copied = 0_u64;
    loop {
        let read = match file.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(error = %err, "reading an update archive on media failed");
                return staging.abort(removed()).await;
            }
        };
        copied += read as u64;
        if copied > size {
            return staging.abort(changed()).await;
        }
        if let Err(response) = staging.push(&buffer[..read]).await {
            return *response;
        }
    }
    if copied != size {
        return staging.abort(changed()).await;
    }
    drop(file);
    staging.import(&state, &source).await
}

#[cfg(test)]
mod tests;
