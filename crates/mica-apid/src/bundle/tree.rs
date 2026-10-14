//! Walking, validating, hashing and syncing a bundle's tree.

use anyhow::{Context, bail};
use aws_lc_rs::digest;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::*;

/// One entry of a staged or installed tree, relative to its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) path: PathBuf,
    pub(super) is_dir: bool,
}

/// Walk `root`, rejecting every entry that is not a regular file or a
/// directory — rule 2, enforced on the staged tree before anything is
/// reachable, which is rule 5's primary mitigation.
pub(super) fn collect(root: &Path) -> anyhow::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    collect_into(root, Path::new(""), &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub(super) fn collect_into(root: &Path, rel: &Path, out: &mut Vec<Entry>) -> anyhow::Result<()> {
    let dir = root.join(rel);
    for entry in fs::read_dir(&dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry?;
        let child_rel = rel.join(entry.file_name());
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", root.join(&child_rel).display()))?;
        let file_type = meta.file_type();
        if file_type.is_dir() {
            out.push(Entry {
                path: child_rel.clone(),
                is_dir: true,
            });
            collect_into(root, &child_rel, out)?;
        } else if file_type.is_file() {
            if meta.nlink() > 1 {
                bail!(Rejection::Hardlink {
                    path: child_rel,
                    links: meta.nlink(),
                });
            }
            out.push(Entry {
                path: child_rel,
                is_dir: false,
            });
        } else {
            bail!(Rejection::IrregularEntry {
                path: child_rel,
                kind: classify(&file_type),
            });
        }
    }
    Ok(())
}

pub(super) fn classify(file_type: &fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_fifo() {
        EntryKind::Fifo
    } else if file_type.is_socket() {
        EntryKind::Socket
    } else if file_type.is_block_device() {
        EntryKind::BlockDevice
    } else if file_type.is_char_device() {
        EntryKind::CharDevice
    } else {
        EntryKind::Unknown
    }
}

/// Step 2 on a staged tree: exactly one readable `index.html` at the
/// root, and no entry that is not a regular file or a directory.
pub(super) fn validate_tree(root: &Path) -> anyhow::Result<Vec<Entry>> {
    let entries = collect(root)?;
    let index = root.join(INDEX_NAME);
    let meta = match fs::symlink_metadata(&index) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => bail!(Rejection::MissingIndex),
        Err(err) => {
            return Err(anyhow::Error::new(err))
                .with_context(|| format!("stat {}", index.display()));
        }
    };
    if !meta.is_file() {
        bail!(Rejection::IndexNotRegularFile);
    }
    if let Err(err) = File::open(&index) {
        bail!(Rejection::IndexUnreadable(err.to_string()));
    }
    Ok(entries)
}

/// Read `mica-ui.json` from the root of a tree. Absent is `None` and valid;
/// present-but-unparsable is a rejection.
pub(super) fn read_manifest(root: &Path) -> anyhow::Result<Option<Manifest>> {
    let path = root.join(MANIFEST_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::Error::new(err))
                .with_context(|| format!("read {}", path.display()));
        }
    };
    match serde_json::from_slice::<Manifest>(&bytes) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(err) => bail!(Rejection::ManifestUnparsable(err.to_string())),
    }
}

/// Class 5: **membership in the served set**, never equality with its
/// `current` member. The trigger is an empty intersection and nothing else, so
/// a bundle declaring only the outgoing major stays compatible for exactly the
/// generation that serves both.
///
/// A bundle with no manifest cannot be checked, and is activated with the
/// check recorded as not run.
pub(super) fn check_compat(
    manifest: Option<&Manifest>,
    served: &[&str],
) -> anyhow::Result<CompatCheck> {
    let Some(manifest) = manifest else {
        return Ok(CompatCheck::NotRun);
    };
    let declared = manifest.api_versions.clone();
    let served: Vec<String> = served.iter().map(|v| (*v).to_string()).collect();
    let compatible = declared.iter().any(|v| served.contains(v));
    if !compatible {
        bail!(Rejection::Incompatible { declared, served });
    }
    Ok(CompatCheck::Ran {
        declared,
        served,
        compatible,
    })
}

/// The modes on the tree the store is about to install: 0755 on
/// directories, 0644 on files.
pub(super) fn apply_modes(root: &Path, entries: &[Entry]) -> anyhow::Result<()> {
    set_mode(root, DIR_MODE)?;
    for entry in entries {
        let path = root.join(&entry.path);
        set_mode(&path, if entry.is_dir { DIR_MODE } else { FILE_MODE })?;
    }
    Ok(())
}

pub(super) fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {mode:o} {}", path.display()))
}

/// Hash a tree: every entry's relative path and every file's length and
/// content, in sorted order, so the digest is a property of the tree rather
/// than of the order the filesystem happened to return it in.
pub(super) fn digest_tree(root: &Path, entries: &[Entry]) -> anyhow::Result<String> {
    let mut hasher = digest::Context::new(&digest::SHA256);
    for entry in entries {
        let path = root.join(&entry.path);
        if entry.is_dir {
            hasher.update(b"d\0");
            hasher.update(entry.path.as_os_str().as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            hasher.update(b"f\0");
            hasher.update(entry.path.as_os_str().as_encoded_bytes());
            hasher.update(b"\0");
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
    }
    Ok(hex::encode(hasher.finish()))
}

/// Hash an installed tree, walking it first. A tree that no longer walks —
/// something planted a symlink in it — is an error here, and every caller
/// turns that into "the digest does not match", because it does not.
pub(super) fn digest_of(root: &Path) -> anyhow::Result<String> {
    let entries = collect(root)?;
    digest_tree(root, &entries)
}

/// Step 3's fsync: without it a power cut can leave `current` resolving
/// to a tree whose data never reached the disk, which is failure class 3.
pub(super) fn fsync_tree(root: &Path, entries: &[Entry]) -> anyhow::Result<()> {
    for entry in entries {
        let path = root.join(&entry.path);
        if entry.is_dir {
            fsync_dir(&path)?;
        } else {
            File::open(&path)
                .with_context(|| format!("open {}", path.display()))?
                .sync_all()
                .with_context(|| format!("fsync {}", path.display()))?;
        }
    }
    fsync_dir(root)
}

pub(super) fn fsync_dir(path: &Path) -> anyhow::Result<()> {
    Ok(mica_fs::sync_dir(path)?)
}

/// Numeric child directory names of `dir`, ascending. A missing `dir` is an
/// empty list, not an error.
pub(super) fn numbered_children(dir: &Path) -> anyhow::Result<Vec<u64>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(err) => {
            return Err(anyhow::Error::new(err))
                .with_context(|| format!("read dir {}", dir.display()));
        }
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(generation) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        found.push(generation);
    }
    found.sort_unstable();
    Ok(found)
}
