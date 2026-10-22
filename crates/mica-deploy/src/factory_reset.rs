//! The factory reset of DATA: the request micad writes, the erase the runkit
//! carries out before the root starts, and the record it leaves.
//!
//! Everything on DATA except `meta/` is removed. `meta/` describes SYSTEM
//! (the firmware receipt, the transaction journal, the catalog checkpoints,
//! the deployment and set records) and holds the request itself, so the
//! request outlives every interrupted erase: a boot that finds it erases
//! again from where the last one stopped. Nothing on SYSTEM or in the boot
//! records is touched.

use anyhow::{Context, Result, ensure};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString},
    fs,
    os::{
        fd::OwnedFd,
        unix::fs::{DirBuilderExt, PermissionsExt},
    },
    path::Path,
};

/// The request, in DATA's `meta/`.
pub const REQUEST: &str = "factory-reset.json";
/// The record of the last completed reset, in DATA's `meta/`.
pub const DONE: &str = "factory-reset-done.json";
/// Written by a runkit that carries out a staged reset: `{"factoryReset":
/// true}`. A device whose runkit predates the reset has no such file, and
/// the reset is not offered there.
pub const CAPABILITIES: &str = "/run/mica/runkit.json";
/// Where a request that cannot be read is moved, so it never erases.
pub const INVALID: &str = "factory-reset.json.invalid";
/// The DATA directories the erase keeps.
const KEPT: &str = "meta";
/// The factory DATA layout the erase leaves, as the image makes it.
const SKELETON: [(&str, u32); 6] = [
    ("state", 0o700),
    ("cache", 0o755),
    ("tmp", 0o755),
    ("var", 0o755),
    ("mica", 0o755),
    ("srv", 0o755),
];
/// Directories nested deeper than this are left in place and counted.
const MAX_DEPTH: usize = 256;
/// How many removal errors are reported by text; the rest are only counted.
const REPORTED_ERRORS: usize = 16;

/// Who asked for a reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Requester {
    /// The console, through apid.
    Console,
    /// An API client, through apid.
    Api,
    /// `micad factory-reset` on the device.
    Cli,
}

/// A staged reset: written by micad, honoured by the runkit at the next boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    /// Unix seconds.
    pub requested: u64,
    pub by: Requester,
    /// The peer or mechanism that asked, for the record.
    pub source: String,
}

/// The record of a completed reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Done {
    pub v: u32,
    pub requested: u64,
    pub by: Requester,
    pub source: String,
    /// Unix seconds, as the device's clock read them early in the boot.
    pub completed: u64,
    /// Entries the erase could not remove; 0 on a clean erase.
    pub unremoved: u64,
}

impl Request {
    fn validate(&self) -> Result<()> {
        ensure!(self.v == 1, "unknown factory reset request version");
        ensure!(
            self.source.len() <= 256 && !self.source.chars().any(char::is_control),
            "invalid factory reset source"
        );
        Ok(())
    }
}

/// Stage `request` in `meta`: written atomically, readable by root alone.
///
/// # Errors
///
/// An invalid request, or one that cannot be written durably.
pub fn request(meta: &Path, request: &Request) -> Result<()> {
    request.validate()?;
    let path = meta.join(REQUEST);
    if let Ok(metadata) = path.symlink_metadata() {
        ensure!(
            metadata.is_file(),
            "{} is not a regular file",
            path.display()
        );
    }
    mica_fs::Replace::new(&path, "pending")?
        .mode(0o600)
        .write(serde_json::to_vec(request)?)?;
    Ok(())
}

/// Whether a reset is staged in `meta`.
#[must_use]
pub fn pending(meta: &Path) -> bool {
    meta.join(REQUEST).symlink_metadata().is_ok()
}

/// The record of the last completed reset in `meta`, if there is one.
///
/// # Errors
///
/// A record that exists and cannot be read.
pub fn done(meta: &Path) -> Result<Option<Done>> {
    let path = meta.join(DONE);
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => Ok(Some(serde_json::from_slice(&mica_fs::read_bounded(
            &path, 4096,
        )?)?)),
    }
}

/// Carry out a staged reset on the DATA filesystem mounted at `data`, before
/// anything else uses it.
///
/// Without a request this does nothing. A request that cannot be read is
/// moved to [`INVALID`] and erases nothing. Otherwise every entry of `data`
/// but `meta/` is removed, the factory layout is made again, [`DONE`]
/// replaces the last record and the request is removed, in that order: an
/// interruption anywhere before the last step leaves the request, and the
/// next boot erases again.
///
/// The erase never crosses into another filesystem. An entry it cannot
/// remove is counted in [`Done::unremoved`] and reported through `log`, and
/// the erase goes on: one bad inode must not stop the device from booting.
///
/// `keepalive` runs before each entry; its error stops the erase, the request
/// staying in place.
///
/// # Errors
///
/// `keepalive`'s error, or DATA's root, `meta/` or the layout failing.
pub fn apply(
    data: &Path,
    now: u64,
    keepalive: &mut dyn FnMut() -> Result<()>,
    log: &mut dyn FnMut(&str),
) -> Result<Option<Done>> {
    let meta = data.join(KEPT);
    let path = meta.join(REQUEST);
    let request = match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => mica_fs::read_bounded(&path, 4096)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<Request>(&bytes)?))
            .and_then(|request| request.validate().map(|()| request)),
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            log(&format!(
                "factory reset request unreadable, not erasing: {error:#}"
            ));
            fs::rename(&path, meta.join(INVALID))?;
            mica_fs::sync_dir(&meta)?;
            return Ok(None);
        }
    };
    let root = open_dir(None, data)?;
    let device = rustix::fs::fstat(&root)?.st_dev;
    let mut erase = Erase {
        device,
        keepalive,
        log,
        unremoved: 0,
    };
    erase.clear(&root, c"DATA", 0, Some(KEPT))?;
    for (name, mode) in SKELETON {
        fs::DirBuilder::new()
            .mode(mode)
            .create(data.join(name))
            .with_context(|| format!("make {name} on DATA"))?;
        fs::set_permissions(data.join(name), fs::Permissions::from_mode(mode))?;
    }
    rustix::fs::fsync(&root)?;
    let done = Done {
        v: 1,
        requested: request.requested,
        by: request.by,
        source: request.source,
        completed: now,
        unremoved: erase.unremoved,
    };
    let record = meta.join(DONE);
    if let Ok(metadata) = record.symlink_metadata()
        && !metadata.is_file()
    {
        fs::remove_dir_all(&record)?;
    }
    mica_fs::Replace::new(&record, "pending")?
        .mode(0o600)
        .write(serde_json::to_vec(&done)?)?;
    fs::remove_file(&path)?;
    mica_fs::sync_dir(&meta)?;
    Ok(Some(done))
}

struct Erase<'a> {
    device: u64,
    keepalive: &'a mut dyn FnMut() -> Result<()>,
    log: &'a mut dyn FnMut(&str),
    unremoved: u64,
}

impl Erase<'_> {
    fn failed(&mut self, what: &str, name: &CStr, error: &dyn std::fmt::Display) {
        self.unremoved += 1;
        if self.unremoved <= REPORTED_ERRORS as u64 {
            (self.log)(&format!(
                "factory reset: {what} {}: {error}",
                name.to_string_lossy()
            ));
        }
    }

    /// Remove every entry of `dir`, called `name`, but `keep`, depth first.
    /// Only `keepalive`'s error stops it.
    fn clear(
        &mut self,
        dir: &OwnedFd,
        name: &CStr,
        depth: usize,
        keep: Option<&str>,
    ) -> Result<()> {
        let mut entries = Vec::new();
        match Dir::read_from(dir) {
            Ok(reader) => {
                for entry in reader {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            self.failed("read", name, &error);
                            break;
                        }
                    };
                    let child = entry.file_name();
                    if child == c"."
                        || child == c".."
                        || keep.is_some_and(|keep| child.to_bytes() == keep.as_bytes())
                    {
                        continue;
                    }
                    entries.push((CString::from(child), entry.file_type()));
                }
            }
            Err(error) => self.failed("read", name, &error),
        }
        for (child, kind) in entries {
            (self.keepalive)()?;
            let kind = if kind == FileType::Unknown {
                match rustix::fs::statat(dir, &child, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                    Err(error) => {
                        self.failed("stat", &child, &error);
                        continue;
                    }
                }
            } else {
                kind
            };
            if kind != FileType::Directory {
                if let Err(error) = rustix::fs::unlinkat(dir, &child, AtFlags::empty()) {
                    self.failed("remove", &child, &error);
                }
                continue;
            }
            let opened = match open_dir(Some(dir), &child) {
                Ok(opened) => opened,
                Err(error) => {
                    self.failed("open", &child, &error);
                    continue;
                }
            };
            match rustix::fs::fstat(&opened) {
                Ok(stat) if stat.st_dev != self.device => {
                    self.failed("keep", &child, &"another filesystem is mounted there");
                    continue;
                }
                Ok(_) if depth >= MAX_DEPTH => {
                    self.failed("keep", &child, &"nested too deep");
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    self.failed("stat", &child, &error);
                    continue;
                }
            }
            self.clear(&opened, &child, depth + 1, None)?;
            drop(opened);
            if let Err(error) = rustix::fs::unlinkat(dir, &child, AtFlags::REMOVEDIR) {
                // A directory left non-empty has its entries counted already.
                if error != rustix::io::Errno::NOTEMPTY {
                    self.failed("remove", &child, &error);
                }
            }
        }
        if let Err(error) = rustix::fs::fsync(dir) {
            (self.log)(&format!(
                "factory reset: sync {}: {error}",
                name.to_string_lossy()
            ));
        }
        Ok(())
    }
}

fn open_dir(at: Option<&OwnedFd>, path: impl rustix::path::Arg) -> std::io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(match at {
        Some(dir) => rustix::fs::openat(dir, path, flags, Mode::empty())?,
        None => rustix::fs::open(path, flags, Mode::empty())?,
    })
}

#[cfg(test)]
mod tests;
