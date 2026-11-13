//! DATA's journal replayed in a mount of its own when its ext4 is smaller
//! than its partition, before the mount the root is given.
//!
//! An online grow of DATA after the root starts (`systemd-growfs` once
//! `systemd-repart` grew the partition) that a power cut stopped leaves a
//! journal to replay. The kernel's online resize, run in the mount that
//! replayed a partial grow, trips over the group count it recovered
//! (`ext4_flex_group_add`, a kernel BUG): a mount and unmount first make the
//! mount the root is given one that recovered nothing.
//!
//! An encrypted DATA is grown here too, after the mount the root is given:
//! its mapping covers the whole partition at each boot, so once
//! `systemd-repart` or `mica-grow-data` grew the partition the ext4 inside
//! is smaller than the mapping, and the root has no fstab line to grow it.

use anyhow::{Context, Result, ensure};
use rustix::mount::{MountFlags, UnmountFlags};
use std::{
    ffi::CStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
};

/// The bytes of the ext4 on `device`, from its superblock as it is on the
/// medium, and of `device` itself.
///
/// # Errors
///
/// `device` cannot be read or does not hold an ext4.
pub fn ext4_extent(device: &mut (impl Read + Seek)) -> Result<(u64, u64)> {
    let mut superblock = [0; 1024];
    device.seek(SeekFrom::Start(1024))?;
    device.read_exact(&mut superblock)?;
    let u32_at = |at: usize| {
        u32::from_le_bytes([
            superblock[at],
            superblock[at + 1],
            superblock[at + 2],
            superblock[at + 3],
        ])
    };
    ensure!(
        u16::from_le_bytes([superblock[0x38], superblock[0x39]]) == 0xef53,
        "DATA is not ext4"
    );
    let log = u32_at(0x18);
    ensure!(log <= 6, "invalid ext4 block size");
    // INCOMPAT_64BIT: the block count's high half is in use.
    let high = if u32_at(0x60) & 0x80 != 0 {
        u64::from(u32_at(0x150))
    } else {
        0
    };
    let blocks = (high << 32) | u64::from(u32_at(0x4));
    let bytes = blocks
        .checked_mul(1024 << log)
        .context("invalid ext4 block count")?;
    Ok((bytes, device.seek(SeekFrom::End(0))?))
}

/// Mount the ext4 on `device` at `target` with `flags` and `data` and
/// unmount it again when it is smaller than `device`, which is what a grow
/// still to finish looks like; answer what was done.
///
/// # Errors
///
/// It cannot be read, mounted or unmounted.
pub fn replay_data(device: &str, target: &str, flags: MountFlags, data: &CStr) -> Result<String> {
    ensure!(target == "/newroot/mnt/data", "only DATA is replayed");
    let (fs, partition) = ext4_extent(&mut File::open(device)?)?;
    if fs >= partition {
        return Ok(String::new());
    }
    rustix::mount::mount(device, target, "ext4", flags, data)?;
    rustix::mount::unmount(target, UnmountFlags::empty())?;
    Ok(format!(
        "ext4 of {fs} bytes on {partition}: journal replayed in a mount of its own"
    ))
}

/// Grow the ext4 mounted at `target` from `device` online to fill
/// `device`, when it is smaller; answer what was done. Run again after a
/// power cut stopped it, it grows what is left, and once grown it does
/// nothing.
///
/// # Errors
///
/// `device` cannot be read, or the kernel refuses the grow.
pub fn grow_data(device: &str, target: &str) -> Result<String> {
    ensure!(target == "/newroot/mnt/data", "only DATA is grown");
    let (fs, size) = ext4_extent(&mut File::open(device)?)?;
    let block = rustix::fs::statvfs(target)?.f_bsize;
    ensure!(block.is_power_of_two(), "invalid ext4 block size");
    let blocks = size / block;
    if blocks * block <= fs {
        return Ok(String::new());
    }
    lifecycle_sys::ext4_resize(File::open(target)?, blocks)
        .with_context(|| format!("grow the ext4 on {device} to {blocks} blocks"))?;
    Ok(format!("ext4 grown from {fs} to {} bytes", blocks * block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn medium(log: u32, low: u32, high: u32, incompat: u32, len: usize) -> Cursor<Vec<u8>> {
        let mut bytes = vec![0; len];
        let sb = 1024;
        bytes[sb + 0x38..sb + 0x3a].copy_from_slice(&0xef53_u16.to_le_bytes());
        bytes[sb + 0x18..sb + 0x1c].copy_from_slice(&log.to_le_bytes());
        bytes[sb + 0x4..sb + 0x8].copy_from_slice(&low.to_le_bytes());
        bytes[sb + 0x60..sb + 0x64].copy_from_slice(&incompat.to_le_bytes());
        bytes[sb + 0x150..sb + 0x154].copy_from_slice(&high.to_le_bytes());
        Cursor::new(bytes)
    }

    #[test]
    fn the_extent_is_read_from_the_superblock() {
        // 4 KiB blocks: 16 of them in 64 KiB, on 128 KiB.
        assert_eq!(
            ext4_extent(&mut medium(2, 16, 0, 0, 131_072)).unwrap(),
            (65_536, 131_072)
        );
        // The high half counts only with INCOMPAT_64BIT.
        assert_eq!(
            ext4_extent(&mut medium(2, 16, 1, 0, 4096)).unwrap().0,
            65_536
        );
        assert_eq!(
            ext4_extent(&mut medium(2, 16, 1, 0x80, 4096)).unwrap().0,
            ((1 << 32) + 16) * 4096
        );
        // A filesystem as large as its device.
        assert_eq!(
            ext4_extent(&mut medium(0, 8, 0, 0, 8192)).unwrap(),
            (8192, 8192)
        );
    }

    #[test]
    fn what_is_not_ext4_is_refused() {
        let mut other = medium(2, 16, 0, 0, 4096);
        other.get_mut()[1024 + 0x38] = 0;
        assert!(ext4_extent(&mut other).is_err());
        assert!(ext4_extent(&mut medium(7, 16, 0, 0, 4096)).is_err());
        assert!(ext4_extent(&mut Cursor::new(vec![0; 1500])).is_err());
    }

    #[test]
    fn a_filesystem_that_fills_its_device_is_not_mounted() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("data");
        std::fs::write(&image, medium(0, 8, 0, 0, 8192).into_inner()).unwrap();
        let image = image.to_str().unwrap();
        let none = c"";
        assert_eq!(
            replay_data(image, "/newroot/mnt/data", MountFlags::empty(), none).unwrap(),
            ""
        );
        assert!(replay_data(image, "/newroot/mnt/other", MountFlags::empty(), none).is_err());
    }
}
