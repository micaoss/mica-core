//! Discarding a mounted filesystem's free space, what `fstrim` does, and
//! erasing a whole block device, what `blkdiscard --secure` does.

use rustix::{
    fd::AsFd,
    io,
    ioctl::{self, Setter, Updater},
};

/// `struct fstrim_range` of `<linux/fs.h>`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct TrimRange {
    start: u64,
    len: u64,
    minlen: u64,
}

const FITRIM: u32 = 0xc0185879;
/// `BLKSECDISCARD`, `_IO(0x12, 125)`: its argument points at `uint64_t[2]`,
/// the byte offset and length.
const BLKSECDISCARD: u32 = 0x127d;

const _: () = {
    assert!(size_of::<TrimRange>() == 24 && align_of::<TrimRange>() == 8);
    assert!(FITRIM == ioctl::opcode::read_write::<TrimRange>(b'X', 121));
    assert!(BLKSECDISCARD == ioctl::opcode::none(0x12, 125));
};

/// Discard every free block of the filesystem `fs` is open on, and return
/// the bytes the filesystem reports trimmed. A filesystem or a device that
/// does not discard answers `EOPNOTSUPP`.
#[allow(unsafe_code)]
pub fn trim_free_space(fs: impl AsFd) -> io::Result<u64> {
    let mut range = TrimRange {
        start: 0,
        len: u64::MAX,
        minlen: 0,
    };
    // SAFETY: FITRIM reads and writes back exactly the initialized repr(C)
    // 24-byte fstrim_range; every field accepts all bit patterns. Updater
    // holds the only borrow and the fd outlives the synchronous call.
    unsafe {
        ioctl::ioctl(fs, Updater::<FITRIM, _>::new(&mut range))?;
    }
    Ok(range.len)
}

/// Securely erase the first `bytes` bytes of the block device `device` is
/// open on (opened for writing): the device erases them, and any copy it
/// moved, beyond recovery. A device that cannot answers `EOPNOTSUPP`.
#[allow(unsafe_code)]
pub fn secure_discard(device: impl AsFd, bytes: u64) -> io::Result<()> {
    let range = [0_u64, bytes];
    // SAFETY: BLKSECDISCARD reads exactly the initialized uint64_t[2] the
    // Setter points at and writes nothing; the fd outlives the synchronous
    // call.
    unsafe { ioctl::ioctl(device, Setter::<BLKSECDISCARD, [u64; 2]>::new(range)) }
}
