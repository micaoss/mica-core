//! The system information as the live-state tree reports it.

use crate::deployment::Status;
use serde_json::{Value as Json, json};

use super::*;

/// Report a single current-domain declaration. An unreadable or malformed
/// declaration leaves the image development-grade without inferred domains.
pub(super) fn marker_domains(text: &str) -> Vec<String> {
    let prefix = format!("{MARKER_DOMAINS_KEY}=");
    let mut declarations = text.lines().filter_map(|line| line.strip_prefix(&prefix));
    let Some(value) = declarations.next() else {
        return Vec::new();
    };
    if declarations.next().is_some() {
        return Vec::new();
    }
    let domains: Vec<_> = value.split_whitespace().collect();
    if domains
        .iter()
        .any(|domain| !matches!(*domain, "boot" | "verity" | "updates"))
        || domains
            .iter()
            .enumerate()
            .any(|(index, domain)| domains[..index].contains(domain))
    {
        return Vec::new();
    }
    domains.into_iter().map(str::to_string).collect()
}

pub(super) use micad_settings::absent;

/// RFC 3339 for `epoch`, or `None` when it is out of range.
pub(super) fn rfc3339(epoch: u64) -> Option<String> {
    let secs = i64::try_from(epoch).ok()?;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .map(|when| when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// `evidence` rendered as the JSON the bus method serves.
///
/// Every member is an object carrying `available`, so a consumer reads one
/// shape whether the fact is there or not, and an absent fact carries the
/// reason it is absent.
#[must_use]
pub fn info_json(
    evidence: &SystemInfoEvidence,
    deployment: Option<&Status>,
    daemon: &DaemonIdentity,
) -> Json {
    let machine_id = match &evidence.machine_id {
        Ok(id) => json!({ "available": true, "id": id }),
        Err(detail) => absent(detail.clone()),
    };
    let board = match &evidence.board {
        Some(board) => json!({ "available": true, "model": board.model, "source": board.source }),
        None => absent("no device-tree model and no DMI product name is exported by this board"),
    };
    let kernel = match (&evidence.kernel_release, &evidence.kernel_version) {
        (None, None) => absent(format!("/{KERNEL_RELEASE_PATH} is not readable")),
        (release, version) => json!({
            "available": true,
            "release": release,
            "version": version,
        }),
    };
    let release = if evidence.os_release.is_empty() {
        absent(format!("/{OS_RELEASE_PATH} is absent or empty"))
    } else {
        let mut root = serde_json::Map::new();
        root.insert("available".to_string(), json!(true));
        for (key, member) in [
            ("NAME", "name"),
            ("ID", "id"),
            ("VERSION", "version"),
            ("VERSION_ID", "versionId"),
            ("PRETTY_NAME", "prettyName"),
            ("BUILD_ID", "buildId"),
            ("IMAGE_ID", "imageId"),
            ("IMAGE_VERSION", "imageVersion"),
        ] {
            if let Some(value) = evidence.os_release.get(key) {
                root.insert(member.to_string(), json!(value));
            }
        }
        Json::Object(root)
    };
    let (system, packages) = match &evidence.manifest {
        None => (
            absent(format!(
                "/{MANIFEST_PATH} is absent; this root was not packed by the image pipeline"
            )),
            absent(format!("/{MANIFEST_PATH} is absent")),
        ),
        Some(manifest) => (
            system_json(manifest, evidence.file_epoch),
            packages_json(manifest),
        ),
    };
    let deployment = match deployment
        .and_then(|status| status.booted().map(|booted| (status, booted)))
    {
        None => absent("the native deployment backend did not report a running deployment"),
        Some((status, booted)) => json!({"available":true,"id":booted.id,"version":booted.version,
            "generation":booted.generation,"kernelId":booted.kernel_id,"kernelRelease":booted.kernel_release,
            "rootfsId":booted.rootfs_id,"confirmed":status.state.current.as_ref() == Some(&booted.id),
            "contentVerified":status.boot.content_verified,"secureBoot":status.boot.secure_boot,
            "backend":status.boot.backend,"bootVerified":status.boot.boot_verified}),
    };
    let uptime = match evidence.uptime_seconds {
        Some(seconds) => json!({ "available": true, "seconds": seconds }),
        None => absent(format!("/{UPTIME_PATH} is not readable")),
    };
    let trust = match &evidence.trust {
        None => absent(format!(
            "/{BAKED_META_MANIFEST_PATH} is absent, so this image provisions no trust anchor \
             and the grade of the material it was built from cannot be read off it -- which is \
             not the same as production"
        )),
        Some(TrustGrade::Production) => json!({ "available": true, "grade": "production" }),
        Some(TrustGrade::Development { domains }) => json!({
            "available": true,
            "grade": "development",
            "developmentDomains": domains,
            "marker": format!("/{BAKED_META_MARKER_PATH}"),
        }),
    };
    json!({
        "machineId": machine_id,
        "board": board,
        "kernel": kernel,
        "release": release,
        "system": system,
        "trust": trust,
        "daemon": {
            "name": daemon.name,
            "version": daemon.version,
        },
        "packages": packages,
        "deployment": deployment,
        "uptime": uptime,
    })
}

/// The `system` member: the image version by way of the system package's
/// manifest row, and the pinned file epoch the root carries.
pub(super) fn system_json(manifest: &Manifest, file_epoch: Option<u64>) -> Json {
    let version_row = SYSTEM_VERSION_PACKAGES
        .iter()
        .find_map(|name| manifest.rows.iter().find(|row| row.name == *name));
    let mut root = serde_json::Map::new();
    if let Some(row) = version_row {
        root.insert("available".to_string(), json!(true));
        root.insert("version".to_string(), json!(row.version));
        root.insert("package".to_string(), json!(row.name));
    } else {
        root.insert("available".to_string(), json!(false));
        root.insert(
            "detail".to_string(),
            json!(format!(
                "no {} row in the manifest",
                SYSTEM_VERSION_PACKAGES.join(" or ")
            )),
        );
    }
    root.insert("fileEpoch".to_string(), file_epoch_json(file_epoch));
    Json::Object(root)
}

/// The `system.fileEpoch` member: the mtime every file in this root carries.
pub(super) fn file_epoch_json(file_epoch: Option<u64>) -> Json {
    let Some(epoch) = file_epoch else {
        return absent(format!(
            "/{MANIFEST_PATH} has no readable mtime, so the epoch every file in this root was \
             pinned to cannot be read back from it"
        ));
    };
    let mut node = serde_json::Map::new();
    node.insert("available".to_string(), json!(true));
    node.insert("epoch".to_string(), json!(epoch));
    if let Some(date) = rfc3339(epoch) {
        node.insert("date".to_string(), json!(date));
    }
    Json::Object(node)
}

/// The `packages` member: every manifest row, with the mica ones marked.
pub(super) fn packages_json(manifest: &Manifest) -> Json {
    let entries: Vec<Json> = manifest
        .rows
        .iter()
        .map(|row| {
            json!({
                "name": row.name,
                "version": row.version,
                "architecture": row.architecture,
                "mica": row.is_mica(),
            })
        })
        .collect();
    json!({
        "available": true,
        "count": manifest.rows.len(),
        "micaCount": manifest.rows.iter().filter(|row| row.is_mica()).count(),
        "malformedRows": manifest.malformed,
        "truncated": manifest.truncated,
        "entries": entries,
    })
}
