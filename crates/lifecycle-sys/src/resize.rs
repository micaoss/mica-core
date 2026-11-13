//! Growing a mounted ext4, what `resize2fs` does online.

use rustix::{
    fd::AsFd,
    io,
    ioctl::{self, Setter},
};

/// `EXT4_IOC_RESIZE_FS`, `_IOW('f', 16, __u64)`: its argument points at the
/// new block count.
const EXT4_IOC_RESIZE_FS: u32 = 0x4008_6610;

const _: () = assert!(EXT4_IOC_RESIZE_FS == ioctl::opcode::write::<u64>(b'f', 16));

/// Grow the ext4 that `fs` (a file or directory on it) is open on to
/// `blocks` of its own blocks. The kernel journals the grow: a power cut
/// leaves a file system that the next mount recovers, smaller than asked.
#[allow(unsafe_code)]
pub fn ext4_resize(fs: impl AsFd, blocks: u64) -> io::Result<()> {
    // SAFETY: EXT4_IOC_RESIZE_FS reads exactly the initialized u64 the
    // Setter points at and writes nothing; the fd outlives the synchronous
    // call.
    unsafe { ioctl::ioctl(fs, Setter::<EXT4_IOC_RESIZE_FS, u64>::new(blocks)) }
}
