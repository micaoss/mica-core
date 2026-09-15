//! The provisioning-document status: one read-only route over what a
//! provisioning document did to this device.
//!
//! A bounded module beside `routes.rs` rather than more of it, on
//! `update_api.rs`'s reasoning: the route shares that file's session gate
//! ([`ApiCredential`]), envelope and response helper, and adds no mapping of
//! its own.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use micad_settings::configuration;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::redact;
use crate::routes::{
    API, ApiCredential, ApiError, AppState, V1_PROVISIONING_STATUS_PATH, api_response,
    bus_api_error,
};

/// The settings subtree the record lives in.
const PROVISIONING_PATH: &str = "provisioning";

/// The settings subtree that answers "is this device claimed".
const ACCESS_PATH: &str = "access";

/// What a provisioning document did to this device.
#[derive(serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisioningStatus {
    /// `version` of the document last applied; `null` when none ever was.
    document_version: Option<u64>,
    /// Canonical digest of the document last applied; `null` with the version.
    document_digest: Option<String>,
    /// The last import ATTEMPT: `source` (`boot` or `media`), `outcome`
    /// (`applied`, `unchanged` or `rejected`), `reason` for a rejection, and
    /// `at`, the device clock's reading when it happened. `null` when no
    /// medium has ever been offered to this device.
    last_import: Option<Value>,
    /// Whether the device still has no administrator credential.
    ///
    /// The same question `GET /api/v1/session` answers with `state: "setup"`,
    /// asked here because it is what decides whether a document offered on a
    /// medium would be applied at all: a claimed device refuses one.
    unclaimed: bool,
    /// The public configuration baked into the verity root, as read.
    baked: Value,
    /// SHA-256 by relative path under `/usr/share/mica/meta/`.
    baked_digests: BTreeMap<String, String>,
    /// What the operator wrote in `/mica/config/updates.json`, projected onto
    /// `update.source`, `update.channel` and `update.policy` and nothing else.
    ///
    /// A field the operator did not write is **absent**; one they wrote as
    /// `null` is **`null`**. Both resolve to the baked default and they are
    /// still two different statements about the document, so the reading is
    /// preserved rather than flattened. `{}` when the document overrides
    /// nothing, which is a factory-reset device.
    operator: Value,
    /// What this device actually runs on after precedence:
    /// `update.source`, `update.channel`, `update.policy`, and `fleet.url` /
    /// `fleet.enabled`.
    ///
    /// The fleet pair is the **baked** value and is not a placeholder for a
    /// resolution not yet written: `/mica/config/`'s fleet document
    /// does not exist, so nothing on the device reads an
    /// operator layer for it either. When that slice lands it extends the
    /// library's resolver; it does not add a second one here.
    effective: Value,
}

/// Read only the allowlisted public files. Never traverse a link or expose an
/// unexpected file merely because it appeared beside the public manifest.
///
/// Addressed by the manifest, and the tree derived from it, so this walk and
/// the resolver that parses the manifest cannot be looking at two different
/// trees. The derivation is checked rather than assumed: a manifest with no
/// grandparent directory is an error here, not a panic, because the path can
/// come from a caller.
fn baked_configuration(manifest_path: &Path) -> Result<(Value, BTreeMap<String, String>)> {
    let root = manifest_path
        .parent()
        .and_then(Path::parent)
        .with_context(|| {
            format!(
                "{} has no baked metadata tree above it",
                manifest_path.display()
            )
        })?;
    let mut files = BTreeMap::new();
    read_baked_files(root, root, &mut files)?;
    let manifest = files
        .get("updates/manifest.json")
        .context("baked manifest is missing")?;
    let document: Value = serde_json::from_slice(manifest).context("parse baked manifest")?;
    ensure!(
        document["schema"] == "mica/meta/v1",
        "unsupported baked manifest schema"
    );
    let digests = files
        .into_iter()
        .map(|(path, bytes)| (path, format!("{:x}", Sha256::digest(bytes))))
        .collect();
    Ok((document, digests))
}

fn read_baked_files(root: &Path, at: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(at)?.is_dir(),
        "baked metadata directory is not a directory"
    );
    let entries = std::fs::read_dir(at).context("read baked metadata directory")?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF-8 baked path")?
            .to_string();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            ensure!(
                relative == "updates",
                "unexpected baked directory {relative}"
            );
            read_baked_files(root, &path, files)?;
        } else {
            ensure!(
                kind.is_file(),
                "baked path {relative} is not a regular file"
            );
            ensure!(
                matches!(relative.as_str(), "updates/manifest.json" | "GENERATED"),
                "unexpected baked file {relative}"
            );
            let bytes = std::fs::read(&path).context("read baked public file")?;
            ensure!(!bytes.is_empty(), "baked file {relative} is empty");
            files.insert(relative, bytes);
        }
    }
    Ok(())
}

/// Read what a provisioning document did to this device.
///
/// Two settings reads plus the public baked configuration: the record micad
/// wrote when it last met a document, and whether an administrator credential
/// exists. Nothing is observed from a medium at request time — the media are staged and read
/// before anything is listening, so a request cannot make a device look at a
/// stick.
#[utoipa::path(
    get,
    path = V1_PROVISIONING_STATUS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "Import history and claim state, the public baked manifest with SHA-256 digests by baked file path, and the operator and effective readings of `update.source`, `update.channel` and `update.policy` beside the baked `fleet` pair. The secret-bearing import document is never returned.", body = ProvisioningStatus),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to answer (`micad_failed`), the baked configuration could not be read (`baked_configuration_unavailable`), or `/mica/config/updates.json` did not read, parse or validate (`configuration_unavailable`) — which is refused rather than answered with the baked value", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_provisioning_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let provisioning = match state.api.get_settings(PROVISIONING_PATH).await {
        Ok(value) => redact::redact(value, PROVISIONING_PATH),
        Err(err) => return bus_api_error(&err, Some(PROVISIONING_PATH)),
    };
    // Redacted too, although only the PRESENCE of the hash is read: this
    // handler must be unable to serve the value even if it were changed to
    // pass the subtree through, which is what makes the rule structural rather
    // than a property of these particular lines.
    let access = match state.api.get_settings(ACCESS_PATH).await {
        Ok(value) => redact::redact(value, ACCESS_PATH),
        Err(err) => return bus_api_error(&err, Some(ACCESS_PATH)),
    };
    let document = provisioning.get("document");
    // ONE path for both halves of layer 1. `meta_manifest` names the file the
    // resolver parses and the tree the digests cover is derived from it, so a
    // response cannot report one device's baked manifest beside another's
    // digests.
    let manifest_path = state.meta_manifest.as_path();
    let (baked, baked_digests) = match baked_configuration(manifest_path) {
        Ok(value) => value,
        Err(err) => {
            return api_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                ApiError::micad("baked_configuration_unavailable", format!("{err:#}")),
            );
        }
    };
    // The operator/effective half, through the library resolver rather than
    // a second reader here (apid links `micad-settings` and cannot link
    // the `micad` binary). One resolver is the whole point: this
    // route and the update path answering differently about the same device is
    // the failure the arrangement exists to prevent.
    let updates_path = state.updates_path.as_path();
    let fleet_path = state.fleet_path.as_path();
    let mut resolved =
        match configuration::provisioning_status_at(manifest_path, updates_path, fleet_path) {
            Ok(value) => value,
            Err(err) => {
                return api_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiError::micad("configuration_unavailable", format!("{err:#}")),
                );
            }
        };
    let operator = resolved["operator"].take();
    let effective = resolved["effective"].take();
    api_response(
        StatusCode::OK,
        ProvisioningStatus {
            baked,
            baked_digests,
            operator,
            effective,
            document_version: document
                .and_then(|record| record.get("appliedVersion"))
                .and_then(Value::as_u64),
            document_digest: document
                .and_then(|record| record.get("appliedDigest"))
                .and_then(Value::as_str)
                .map(str::to_string),
            last_import: document
                .and_then(|record| record.get("lastImport"))
                .cloned(),
            // A device is claimed exactly when it carries an administrator
            // password hash, which is the same predicate the session route
            // reads to decide `setup`. Read as "is the hash there", never as
            // "what is the hash": the value arrives redacted and its presence
            // is all this needs.
            unclaimed: access
                .get("webAdmin")
                .and_then(|admin| admin.get("password_hash"))
                .is_none(),
        },
    )
}
