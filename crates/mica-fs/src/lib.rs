//! `mica-fs` — the durable-file primitives every mica daemon shares: an atomic
//! replace, a directory sync and bounded reads.
//!
//! One implementation, because each copy of these had drifted: one forgot the
//! directory sync, one the mode of a leftover temporary file, one followed a
//! symlink planted under the temporary name.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Attach the operation and the path to an I/O error, keeping its kind.
fn at(operation: &str, path: &Path) -> impl FnOnce(io::Error) -> io::Error {
    let context = format!("{operation} {}", path.display());
    move |err| io::Error::new(err.kind(), format!("{context}: {err}"))
}

/// The directory `path` lives in; `.` for a bare file name.
fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Replace a file atomically: a temporary sibling written, synced and renamed
/// over the target, then the directory synced so the rename survives a power
/// cut.
///
/// The temporary name is fixed per caller, so a run interrupted before the
/// rename leaves exactly one leftover that the next run removes. It is created
/// exclusively: whatever sits under the name, a symlink included, is unlinked
/// first and never written through.
#[must_use = "a Replace does nothing until `write` is called"]
pub struct Replace<'a> {
    path: &'a Path,
    temp: PathBuf,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
}

impl<'a> Replace<'a> {
    /// Replace `path` through the hidden sibling `.<name>.<tag>`.
    pub fn new(path: &'a Path, tag: &str) -> io::Result<Self> {
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} has no file name", path.display()),
            )
        })?;
        let mut temp = OsString::from(".");
        temp.push(name);
        temp.push(".");
        temp.push(tag);
        Ok(Self::via(path, parent_of(path).join(temp)))
    }

    /// Replace `path` through `temp`, which must be in the same directory:
    /// for a caller whose recovery recognises its own temporary names.
    pub fn via(path: &'a Path, temp: PathBuf) -> Self {
        Self {
            path,
            temp,
            mode: None,
            uid: None,
            gid: None,
        }
    }

    /// Give the file `mode`, set on the descriptor so the umask does not
    /// narrow it. Without it the file is created `0666` less the umask.
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Give the file `(uid, gid)` before it becomes visible under its name.
    pub fn owner(mut self, owner: Option<(u32, u32)>) -> Self {
        if let Some((uid, gid)) = owner {
            self.uid = Some(uid);
            self.gid = Some(gid);
        }
        self
    }

    /// Give the file group `gid`, keeping the owner, before it becomes
    /// visible under its name.
    pub fn group(mut self, gid: Option<u32>) -> Self {
        if gid.is_some() {
            self.gid = gid;
        }
        self
    }

    /// Write `bytes` and make them the file's content.
    pub fn write(self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        let temp = self.temp.as_path();
        match fs::remove_file(temp) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                return Err(at("remove", temp)(err));
            }
            _ => {}
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(self.mode.unwrap_or(0o666))
            .open(temp)
            .map_err(at("create", temp))?;
        if let Some(mode) = self.mode {
            file.set_permissions(fs::Permissions::from_mode(mode))
                .map_err(at("set mode on", temp))?;
        }
        if self.uid.is_some() || self.gid.is_some() {
            std::os::unix::fs::fchown(&file, self.uid, self.gid)
                .map_err(at("set owner on", temp))?;
        }
        file.write_all(bytes.as_ref()).map_err(at("write", temp))?;
        file.sync_all().map_err(at("flush", temp))?;
        drop(file);
        fs::rename(temp, self.path).map_err(at("rename to", self.path))?;
        sync_dir(parent_of(self.path))
    }
}

/// Sync a directory, which is what makes a create, rename or unlink inside it
/// durable.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(at("sync directory", path))
}

/// Read a regular file of at most `limit` bytes. A symlink, a device or a
/// file larger than `limit` is refused, including one that grows while it is
/// read.
pub fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let invalid = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {what}", path.display()),
        )
    };
    let metadata = fs::symlink_metadata(path).map_err(at("stat", path))?;
    if !metadata.is_file() {
        return Err(invalid("not a regular file"));
    }
    if metadata.len() > limit {
        return Err(invalid(&format!("larger than {limit} bytes")));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(at("open", path))?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(at("read", path))?;
    if bytes.len() as u64 > limit {
        return Err(invalid(&format!("grew beyond {limit} bytes")));
    }
    Ok(bytes)
}

/// A one-value text file -- a sysfs attribute, a device-tree string -- with
/// surrounding whitespace and NULs trimmed. `None` when it cannot be read,
/// or holds nothing.
pub fn read_trimmed(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests;
