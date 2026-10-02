//! Bounded reads, synced writes and the boot partition the policy names.

use crate::boot::selected_entry;
use anyhow::{Context, Result, ensure};
use mica_fs::sync_dir;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Resolve the boot partition on the disk holding the authenticated SYSTEM
/// partition, by the GPT numbers the signed boot policy names.
///
/// On a FIT board the partition is also held to the policy's geometry -- its
/// start and length -- before anything is written to it: a disk's geometry is
/// fixed at the factory, and a later kernel whose policy names another must
/// refuse rather than write records where this disk does not keep them.
pub fn boot_partition(system: &Path, board: &crate::board::BoardFacts) -> Result<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    ensure!(
        fs::read_to_string(system.join("partition"))?.trim() == board.partitions.system.to_string(),
        "invalid SYSTEM partition"
    );
    let parent = system.parent().context("SYSTEM has no physical disk")?;
    let boot = board.partitions.boot.to_string();
    let mut partitions = Vec::new();
    for child in fs::read_dir(parent)? {
        let child = child?.path();
        if fs::read_to_string(child.join("partition")).is_ok_and(|number| number.trim() == boot) {
            partitions.push(child);
        }
    }
    ensure!(
        partitions.len() == 1,
        "boot partition is absent or ambiguous"
    );
    let physical = &partitions[0];
    if let Some(layout) = board.records {
        ensure!(
            fs::read_to_string(physical.join("start"))?.trim() == layout.start_sector().to_string()
                && fs::read_to_string(physical.join("size"))?.trim()
                    == layout.sectors().to_string(),
            "FIRMWARE geometry differs from the signed boot policy"
        );
    }
    let device = Path::new("/dev").join(physical.file_name().context("missing boot device")?);
    ensure!(
        device.symlink_metadata()?.file_type().is_block_device(),
        "boot partition is not a block device"
    );
    Ok(device)
}

pub fn valid_id(id: &str) -> Result<()> {
    ensure!(
        selected_entry(&format!("mica-{id}.conf"))? == id,
        "invalid ID"
    );
    Ok(())
}

/// Replace `path` through `<stem>.pending`, the name [`DeploymentStore::collect`]
/// recognises as an interrupted write.
///
/// [`DeploymentStore::collect`]: super::DeploymentStore::collect
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        ensure!(
            path.symlink_metadata()?.is_file(),
            "destination is not a regular file"
        );
    }
    Ok(mica_fs::Replace::via(path, path.with_extension("pending")).write(bytes)?)
}

pub(super) struct Collection {
    pub(super) files: Vec<PathBuf>,
    pub(super) directories: Vec<PathBuf>,
}

impl Collection {
    pub(super) fn reclaimed_bytes(&self, destination: &Path) -> Result<u64> {
        let device = destination.metadata()?.dev();
        self.files.iter().try_fold(0_u64, |bytes, file| {
            let metadata = file.symlink_metadata()?;
            // Hard-linked objects may still occupy blocks after unlinking.
            Ok(bytes
                + if metadata.dev() == device && metadata.nlink() == 1 {
                    metadata.blocks() * 512
                } else {
                    0
                })
        })
    }

    pub(super) fn apply(self) -> Result<usize> {
        let count = self.files.len();
        for file in self.files {
            fs::remove_file(&file)?;
            sync_dir(file.parent().context("missing collection parent")?)?;
        }
        for directory in self.directories {
            fs::remove_dir(&directory)?;
            sync_dir(directory.parent().context("missing collection parent")?)?;
        }
        Ok(count)
    }
}
