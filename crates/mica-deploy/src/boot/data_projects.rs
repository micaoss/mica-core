//! DATA's project quotas, set before the root starts.
//!
//! Project 100 is system and user data (`mica`, `srv`), 101 the disposable
//! data (`cache`, `tmp`, `var`), bounded to one eighth of DATA, and 102 the
//! containers. Every boot, idempotently: the directories are made when
//! missing, each carries its project with inheritance, and the limits are
//! set again from the filesystem's current size.

use anyhow::{Context, Result, ensure};
use rustix::fs::{Gid, Mode, OFlags, Uid};
use std::{fs, os::unix::fs::DirBuilderExt, path::Path};

/// Each project directory: name, mode, project.
const PROJECT_DIRECTORIES: [(&str, u32, u32); 6] = [
    ("mica", 0o755, 100),
    ("srv", 0o755, 100),
    ("cache", 0o755, 101),
    ("tmp", 0o1777, 101),
    ("var", 0o755, 101),
    ("containers", 0o711, 102),
];

/// Project 101's limits: one eighth of DATA's blocks, in KiB, within
/// 32..256 MiB, and one eighth of its inodes, within 2048..16384.
#[must_use]
pub fn variable_budget(block_size: u64, blocks: u64, inodes: u64) -> (u64, u64) {
    let kib = block_size.saturating_mul(blocks) / 1024 / 8;
    (kib.clamp(32768, 262_144), (inodes / 8).clamp(2048, 16384))
}

/// Lay out DATA's project directories under `data`, the mounted DATA root,
/// and set the three projects' limits.
pub fn data_projects(data: &Path) -> Result<()> {
    for (name, mode, project) in PROJECT_DIRECTORIES {
        let path = data.join(name);
        match path.symlink_metadata() {
            Ok(metadata) => ensure!(metadata.is_dir(), "{} is not a directory", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(mode).create(&path)?;
            }
            Err(error) => return Err(error.into()),
        }
        let dir = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| format!("open {}", path.display()))?;
        rustix::fs::fchown(&dir, Some(Uid::ROOT), Some(Gid::ROOT))?;
        rustix::fs::fchmod(&dir, Mode::from_raw_mode(mode))?;
        lifecycle_sys::set_project_inherit(&dir, project)
            .with_context(|| format!("set project {project} on {}", path.display()))?;
    }
    let root = fs::File::open(data)?;
    let space = rustix::fs::fstatvfs(&root)?;
    let (kib, inodes) = variable_budget(space.f_frsize, space.f_blocks, space.f_files);
    for (project, kib, inodes) in [(100, 0, 0), (101, kib, inodes), (102, 0, 0)] {
        lifecycle_sys::set_project_limits(&root, project, kib, inodes)
            .with_context(|| format!("set the limits of project {project}"))?;
    }
    root.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
