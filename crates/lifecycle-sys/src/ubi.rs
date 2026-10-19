//! UBI volume management on a UBI device or volume character device, and the
//! bad-block query of an MTD character device.
use super::*;
use rustix::ioctl::{IoctlOutput, Opcode, Setter};

/// The longest UBI volume name, in bytes.
pub const UBI_MAX_VOLUME_NAME: usize = 127;
/// The most volumes one rename request carries.
pub const UBI_MAX_RENAME: usize = 32;

/// What a volume's data is: rewritten in place, or written once whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UbiVolumeKind {
    Dynamic,
    Static,
}

#[repr(C, packed)]
struct MkvolRequest {
    volume: i32,
    alignment: i32,
    bytes: i64,
    kind: i8,
    flags: u8,
    name_length: i16,
    padding: [u8; 4],
    name: [u8; UBI_MAX_VOLUME_NAME + 1],
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RenameEntry {
    volume: i32,
    name_length: i16,
    padding: [u8; 2],
    name: [u8; UBI_MAX_VOLUME_NAME + 1],
}

#[repr(C, packed)]
struct RenameRequest {
    count: i32,
    padding: [u8; 12],
    entries: [RenameEntry; UBI_MAX_RENAME],
}

#[repr(C, packed)]
struct LebChangeRequest {
    leb: i32,
    bytes: i32,
    /// Obsolete; the kernel ignores it.
    kind: i8,
    padding: [u8; 7],
}

const MKVOL: Opcode = 0x4098_6f00;
const RMVOL: Opcode = 0x4004_6f01;
const RNVOL: Opcode = 0x5110_6f03;
const VOLUP: Opcode = 0x4008_4f00;
const EBCH: Opcode = 0x4004_4f02;
const VOLCRBLK: Opcode = 0x4080_4f07;
const VOLRMBLK: Opcode = 0x0000_4f08;
const MEMGETBADBLOCK: Opcode = 0x4008_4d0b;
const VOLUME_AUTO: i32 = -1;
const DYNAMIC: i8 = 3;
const STATIC: i8 = 4;

const _: () = {
    assert!(size_of::<MkvolRequest>() == 152 && align_of::<MkvolRequest>() == 1);
    assert!(std::mem::offset_of!(MkvolRequest, bytes) == 8);
    assert!(std::mem::offset_of!(MkvolRequest, kind) == 16);
    assert!(std::mem::offset_of!(MkvolRequest, name_length) == 18);
    assert!(std::mem::offset_of!(MkvolRequest, name) == 24);
    assert!(size_of::<RenameEntry>() == 136);
    assert!(size_of::<RenameRequest>() == 4368 && align_of::<RenameRequest>() == 1);
    assert!(std::mem::offset_of!(RenameRequest, entries) == 16);
    assert!(size_of::<LebChangeRequest>() == 16);
    assert!(MKVOL == ioctl::opcode::write::<MkvolRequest>(b'o', 0));
    assert!(RMVOL == ioctl::opcode::write::<i32>(b'o', 1));
    assert!(RNVOL == ioctl::opcode::write::<RenameRequest>(b'o', 3));
    assert!(VOLUP == ioctl::opcode::write::<i64>(b'O', 0));
    // The kernel's request number encodes an `__s32` although the argument
    // is the whole change request.
    assert!(EBCH == ioctl::opcode::write::<i32>(b'O', 2));
    assert!(VOLCRBLK == ioctl::opcode::write::<[u8; 128]>(b'O', 7));
    assert!(VOLRMBLK == ioctl::opcode::none(b'O', 8));
    assert!(MEMGETBADBLOCK == ioctl::opcode::write::<i64>(b'M', 11));
};

fn volume_name(name: &str) -> io::Result<([u8; UBI_MAX_VOLUME_NAME + 1], i16)> {
    if name.is_empty() || name.len() > UBI_MAX_VOLUME_NAME || name.contains('\0') {
        return Err(io::Errno::INVAL);
    }
    let mut bytes = [0; UBI_MAX_VOLUME_NAME + 1];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    Ok((bytes, name.len() as i16))
}

fn volume_id(volume: u32) -> io::Result<i32> {
    i32::try_from(volume).map_err(|_| io::Errno::INVAL)
}

/// Create a volume of `bytes` named `name` on the UBI device `device`; the
/// kernel numbers it, and the number is returned.
#[allow(unsafe_code)]
pub fn ubi_mkvol(
    device: impl AsFd,
    name: &str,
    bytes: u64,
    kind: UbiVolumeKind,
) -> io::Result<u32> {
    let (name, name_length) = volume_name(name)?;
    let mut request = MkvolRequest {
        volume: VOLUME_AUTO,
        alignment: 1,
        bytes: i64::try_from(bytes)
            .ok()
            .filter(|bytes| *bytes > 0)
            .ok_or(io::Errno::INVAL)?,
        kind: match kind {
            UbiVolumeKind::Dynamic => DYNAMIC,
            UbiVolumeKind::Static => STATIC,
        },
        flags: 0,
        name_length,
        padding: [0; 4],
        name,
    };
    // SAFETY: UBI_IOCMKVOL reads the packed 152-byte ubi_mkvol_req and writes
    // the assigned volume number back into its first field. Every field is
    // initialized; Updater holds the only borrow through the synchronous call.
    unsafe {
        ioctl::ioctl(device, ioctl::Updater::<MKVOL, _>::new(&mut request))?;
    }
    u32::try_from(request.volume).map_err(|_| io::Errno::PROTO)
}

/// Remove volume `volume` of the UBI device `device`.
#[allow(unsafe_code)]
pub fn ubi_rmvol(device: impl AsFd, volume: u32) -> io::Result<()> {
    let volume = volume_id(volume)?;
    // SAFETY: UBI_IOCRMVOL reads one __s32 volume number and writes nothing.
    unsafe { ioctl::ioctl(device, Setter::<RMVOL, _>::new(volume)) }
}

/// Rename volumes of the UBI device `device` in one volume-table update.
#[allow(unsafe_code)]
pub fn ubi_rename(device: impl AsFd, renames: &[(u32, &str)]) -> io::Result<()> {
    if renames.is_empty() || renames.len() > UBI_MAX_RENAME {
        return Err(io::Errno::INVAL);
    }
    let blank = RenameEntry {
        volume: 0,
        name_length: 0,
        padding: [0; 2],
        name: [0; UBI_MAX_VOLUME_NAME + 1],
    };
    let mut request = RenameRequest {
        count: renames.len() as i32,
        padding: [0; 12],
        entries: [blank; UBI_MAX_RENAME],
    };
    for (entry, (volume, name)) in request.entries.iter_mut().zip(renames) {
        let (name, name_length) = volume_name(name)?;
        *entry = RenameEntry {
            volume: volume_id(*volume)?,
            name_length,
            padding: [0; 2],
            name,
        };
    }
    // SAFETY: UBI_IOCRNVOL reads the packed 4368-byte ubi_rnvol_req, every
    // byte initialized, and writes nothing back.
    unsafe { ioctl::ioctl(device, Setter::<RNVOL, _>::new(request)) }
}

/// Start replacing the whole of the volume behind `volume` with `bytes`
/// bytes, which the caller then writes to the same descriptor.
#[allow(unsafe_code)]
pub fn ubi_volume_update(volume: impl AsFd, bytes: u64) -> io::Result<()> {
    let bytes = i64::try_from(bytes).map_err(|_| io::Errno::INVAL)?;
    // SAFETY: UBI_IOCVOLUP reads one __s64 length and writes nothing.
    unsafe { ioctl::ioctl(volume, Setter::<VOLUP, _>::new(bytes)) }
}

/// Start replacing logical erase block `leb` of the volume behind `volume`
/// atomically with `bytes` bytes, which the caller then writes to the same
/// descriptor: the block holds the old data or the new, never a mix.
#[allow(unsafe_code)]
pub fn ubi_leb_change(volume: impl AsFd, leb: u32, bytes: u32) -> io::Result<()> {
    let request = LebChangeRequest {
        leb: volume_id(leb)?,
        bytes: i32::try_from(bytes).map_err(|_| io::Errno::INVAL)?,
        kind: 0,
        padding: [0; 7],
    };
    // SAFETY: UBI_IOCEBCH reads the packed 16-byte ubi_leb_change_req, every
    // byte initialized, and writes nothing back.
    unsafe { ioctl::ioctl(volume, Setter::<EBCH, _>::new(request)) }
}

/// Create the read-only `ubiblock` of the volume behind `volume`.
#[allow(unsafe_code)]
pub fn ubi_block_create(volume: impl AsFd) -> io::Result<()> {
    // SAFETY: UBI_IOCVOLCRBLK reads the 128-byte, all-padding
    // ubi_blkcreate_req and writes nothing back.
    unsafe { ioctl::ioctl(volume, Setter::<VOLCRBLK, _>::new([0_u8; 128])) }
}

/// Remove the `ubiblock` of the volume behind `volume`.
#[allow(unsafe_code)]
pub fn ubi_block_remove(volume: impl AsFd) -> io::Result<()> {
    // SAFETY: UBI_IOCVOLRMBLK takes no argument; NoArg passes null.
    unsafe { ioctl::ioctl(volume, NoArg::<VOLRMBLK>::new()) }
}

/// MEMGETBADBLOCK: an offset in, the answer in the return value.
struct BadBlock(i64);

// SAFETY: MEMGETBADBLOCK reads one loff_t through the pointer and returns 0
// (good) or 1 (bad) as the call's result; it writes nothing into userspace.
#[allow(unsafe_code)]
unsafe impl ioctl::Ioctl for BadBlock {
    type Output = bool;
    const IS_MUTATING: bool = false;

    fn opcode(&self) -> Opcode {
        MEMGETBADBLOCK
    }

    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        (&raw mut self.0).cast()
    }

    unsafe fn output_from_ptr(
        out: IoctlOutput,
        _: *mut std::ffi::c_void,
    ) -> io::Result<Self::Output> {
        match out {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(io::Errno::PROTO),
        }
    }
}

/// Whether the erase block at byte `offset` of the MTD device behind `mtd`
/// is marked bad.
#[allow(unsafe_code)]
pub fn mtd_bad_block(mtd: impl AsFd, offset: u64) -> io::Result<bool> {
    let offset = i64::try_from(offset).map_err(|_| io::Errno::INVAL)?;
    // SAFETY: see the Ioctl implementation; the offset lives on this frame
    // through the synchronous call.
    unsafe { ioctl::ioctl(mtd, BadBlock(offset)) }
}
