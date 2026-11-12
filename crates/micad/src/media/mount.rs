//! Mounting a media volume: read-only, no device nodes, no set-id, no
//! execution, and for ext no journal replay.

use std::ffi::CStr;
use std::io;
use std::path::Path;

use rustix::mount::{MountFlags, UnmountFlags};

/// The kernel calls media mounts make, behind a seam for the tests.
pub trait Mounter: Send + Sync {
    /// Mount the `filesystem` on `device` at `target`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn mount(&self, device: &Path, target: &Path, filesystem: &str) -> io::Result<()>;

    /// Unmount `target`; `detach` only for a device that is gone.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, `EBUSY` while something uses it.
    fn unmount(&self, target: &Path, detach: bool) -> io::Result<()>;

    /// The bytes free to an unprivileged writer on the mount at `target`.
    fn available(&self, target: &Path) -> Option<u64>;
}

/// The kernel's options for `filesystem`: its driver and mount data.
#[must_use]
pub fn options(filesystem: &str) -> Option<(&'static str, &'static CStr)> {
    match filesystem {
        "vfat" => Some(("vfat", c"utf8,uid=0,gid=0,fmask=0133,dmask=0022")),
        "exfat" => Some(("exfat", c"iocharset=utf8,uid=0,gid=0,fmask=0133,dmask=0022")),
        "ext2" | "ext3" | "ext4" => Some(("ext4", c"noload")),
        _ => None,
    }
}

/// The running kernel.
pub struct HostMounter;

impl Mounter for HostMounter {
    fn mount(&self, device: &Path, target: &Path, filesystem: &str) -> io::Result<()> {
        let (driver, data) = options(filesystem).ok_or(io::ErrorKind::Unsupported)?;
        let flags =
            MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;
        rustix::mount::mount(device, target, driver, flags, data).map_err(io::Error::from)
    }

    fn unmount(&self, target: &Path, detach: bool) -> io::Result<()> {
        let flags = if detach {
            UnmountFlags::DETACH
        } else {
            UnmountFlags::empty()
        };
        rustix::mount::unmount(target, flags).map_err(io::Error::from)
    }

    fn available(&self, target: &Path) -> Option<u64> {
        let stat = rustix::fs::statvfs(target).ok()?;
        stat.f_bavail.checked_mul(stat.f_frsize)
    }
}
