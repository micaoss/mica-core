//! The factory reset of version 2 (`docs/mica-core.md` 3.7): DATA is made
//! again, not emptied, and the device returns to the deployment that was
//! running and confirmed when the reset was staged.
//!
//! What must outlive DATA is on SYSTEM, in `factory-reset/`: the request and
//! a snapshot of the files of DATA's `meta/` that describe SYSTEM (the
//! firmware receipt, the catalog checkpoints and the deployment state). The
//! running system stages both under the
//! transaction lock, the snapshot first, so a request is never without its
//! snapshot. The runkit, which mounts SYSTEM read-only, makes DATA again and
//! restores the snapshot at every boot that finds the request; the first boot
//! of the root after one finishes the reset on SYSTEM and removes the request
//! last. An interruption anywhere before that leaves the request, and the
//! next boot makes DATA again.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

use super::{Request, skeleton};
use crate::deployments::{atomic_write, read_bounded, sync_dir};

/// The reset's directory on SYSTEM.
pub const SYSTEM_DIR: &str = "factory-reset";
/// The request, in [`SYSTEM_DIR`].
const SYSTEM_REQUEST: &str = "request.json";
/// The snapshot of DATA's `meta/`, in [`SYSTEM_DIR`].
const SNAPSHOT: &str = "snapshot";
/// The files of DATA's `meta/` a reset keeps. Every other one goes with DATA.
pub const SNAPSHOT_FILES: [&str; 3] = ["firmware.json", "catalog.json", "deployments.json"];
/// The most bytes a snapshot file holds.
const MAX_SNAPSHOT_FILE: u64 = 1024 * 1024;
/// What the runkit did at this boot, for the root to finish: [`Progress`].
pub const PROGRESS: &str = "/run/mica/factory-reset.json";

/// What the runkit did to DATA at this boot, in [`PROGRESS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub v: u32,
    /// The request's, to tell this reset from another.
    pub requested: u64,
    /// DATA was made again; `false`, its format failed and it was erased in
    /// place.
    pub formatted: bool,
    /// Entries the erase in place could not remove.
    pub unremoved: u64,
}

/// The reset staged on the SYSTEM mounted at `system`, if there is one.
///
/// # Errors
///
/// A request that exists and cannot be read, or is not of version 2.
pub fn staged(system: &Path) -> Result<Option<Request>> {
    let path = system.join(SYSTEM_DIR).join(SYSTEM_REQUEST);
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => {
            let request: Request = serde_json::from_slice(&read_bounded(&path, 4096)?)?;
            request.validate()?;
            ensure!(request.v == 2, "a factory reset on SYSTEM is of version 2");
            Ok(Some(request))
        }
    }
}

/// Stage `request` on the SYSTEM mounted at `system`, with the snapshot of
/// DATA's `meta`. The caller holds the transaction lock and a writable
/// SYSTEM.
///
/// # Errors
///
/// A reset staged already, an invalid request, or a write that fails.
pub fn stage(system: &Path, meta: &Path, request: &Request) -> Result<()> {
    request.validate()?;
    ensure!(request.v == 2, "a factory reset on SYSTEM is of version 2");
    ensure!(
        staged(system)?.is_none(),
        "a factory reset is staged already"
    );
    let directory = system.join(SYSTEM_DIR);
    let snapshot = directory.join(SNAPSHOT);
    // A snapshot without its request is one an interrupted staging left.
    remove_snapshot(&directory)?;
    fs::create_dir_all(&snapshot)?;
    for name in SNAPSHOT_FILES {
        let source = meta.join(name);
        if source.symlink_metadata().is_err() {
            continue;
        }
        let bytes =
            read_bounded(&source, MAX_SNAPSHOT_FILE).with_context(|| format!("snapshot {name}"))?;
        atomic_write(&snapshot.join(name), &bytes)?;
    }
    sync_dir(&snapshot)?;
    sync_dir(&directory)?;
    sync_dir(system)?;
    mica_fs::Replace::new(&directory.join(SYSTEM_REQUEST), "pending")?
        .mode(0o600)
        .write(serde_json::to_vec(request)?)?;
    Ok(())
}

/// Remove the reset staged on the SYSTEM mounted at `system`, the request
/// first: cancelling it, or the last step of finishing it. Answer whether
/// one was staged. The caller holds the transaction lock and a writable
/// SYSTEM.
///
/// # Errors
///
/// A removal that fails.
pub fn unstage(system: &Path) -> Result<bool> {
    let directory = system.join(SYSTEM_DIR);
    let request = directory.join(SYSTEM_REQUEST);
    let was = request.symlink_metadata().is_ok();
    if was {
        fs::remove_file(&request)?;
        sync_dir(&directory)?;
    }
    remove_snapshot(&directory)?;
    if directory.symlink_metadata().is_ok() {
        fs::remove_dir(&directory)?;
        sync_dir(system)?;
    }
    Ok(was)
}

fn remove_snapshot(directory: &Path) -> Result<()> {
    let snapshot = directory.join(SNAPSHOT);
    if snapshot.symlink_metadata().is_err() {
        return Ok(());
    }
    for entry in fs::read_dir(&snapshot)? {
        let entry = entry?;
        ensure!(entry.file_type()?.is_file(), "unexpected snapshot entry");
        fs::remove_file(entry.path())?;
    }
    fs::remove_dir(&snapshot)?;
    sync_dir(directory)?;
    Ok(())
}

/// Make the factory layout on the DATA mounted at `data`, which the runkit
/// has just made again or erased, and restore the snapshot of the reset
/// staged on the SYSTEM mounted at `system` into its `meta/`.
///
/// # Errors
///
/// The layout or a snapshot file that cannot be written.
pub fn restore(system: &Path, data: &Path) -> Result<()> {
    skeleton(data)?;
    let snapshot = system.join(SYSTEM_DIR).join(SNAPSHOT);
    let meta = data.join("meta");
    for name in SNAPSHOT_FILES {
        let source = snapshot.join(name);
        if source.symlink_metadata().is_err() {
            continue;
        }
        atomic_write(&meta.join(name), &read_bounded(&source, MAX_SNAPSHOT_FILE)?)?;
    }
    sync_dir(&meta)?;
    sync_dir(data)?;
    Ok(())
}

/// What the runkit did at this boot, if it carried out a reset.
///
/// # Errors
///
/// A record that exists and cannot be read.
pub fn progress(path: &Path) -> Result<Option<Progress>> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Ok(Some(serde_json::from_slice(&read_bounded(path, 4096)?)?)),
    }
}
