//! Device-mapper: the status, table and removal ioctls, bounded and typed.

use rustix::{
    fd::AsFd,
    io,
    ioctl::{self, Updater},
};

use super::*;

// Linux DM v4 uses a 312-byte request header but returns only 305 header bytes
// for DEV_STATUS/REMOVE. TABLE_STATUS appends 8-byte-aligned target records.
pub(super) const DM_STATUS_REQUEST: u32 = 0xc138_fd07;
pub(super) const DM_TABLE_REQUEST: u32 = 0xc138_fd0c;
pub(super) const DM_REMOVE_REQUEST: u32 = 0xc138_fd04;
pub(super) const DM_READONLY: u32 = 1;
pub(super) const DM_TABLE: u32 = 1 << 4;
pub(super) const DM_ACTIVE: u32 = 1 << 5;
pub(super) const DM_FULL: u32 = 1 << 8;
pub(super) const DM_UEVENT: u32 = 1 << 13;
pub(super) const DM_CAPACITY: usize = 65536;

#[repr(C)]
pub(super) struct DmHeader {
    pub(super) version: [u32; 3],
    pub(super) data_size: u32,
    pub(super) data_start: u32,
    pub(super) target_count: u32,
    pub(super) open_count: i32,
    pub(super) flags: u32,
    pub(super) event_nr: u32,
    pub(super) padding: u32,
    pub(super) dev: u64,
    pub(super) name: [u8; 128],
    pub(super) uuid: [u8; 129],
    pub(super) data: [u8; 7],
}
#[repr(C)]
pub(super) struct DmBuffer {
    pub(super) header: DmHeader,
    pub(super) data: [u8; DM_CAPACITY - 312],
}
const _: () = {
    assert!(size_of::<DmHeader>() == 312 && align_of::<DmHeader>() == 8);
    assert!(std::mem::offset_of!(DmHeader, dev) == 40);
    assert!(std::mem::offset_of!(DmHeader, name) == 48);
    assert!(std::mem::offset_of!(DmHeader, uuid) == 176);
    assert!(std::mem::offset_of!(DmHeader, data) == 305);
    assert!(size_of::<DmBuffer>() == DM_CAPACITY && align_of::<DmBuffer>() == 8);
    assert!(std::mem::offset_of!(DmBuffer, data) == 312);
    assert!(DM_STATUS_REQUEST == ioctl::opcode::read_write::<DmHeader>(0xfd, 7));
    assert!(DM_TABLE_REQUEST == ioctl::opcode::read_write::<DmHeader>(0xfd, 12));
    assert!(DM_REMOVE_REQUEST == ioctl::opcode::read_write::<DmHeader>(0xfd, 4));
};

/// Checked active, read-only DM identity. Event/count comparisons detect table
/// changes between status and table/removal requests; this is not a release token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmStatus {
    pub device: u64,
    pub name: String,
    pub uuid: String,
    pub targets: u32,
    pub open_count: i32,
    pub event: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmTarget {
    pub sector: u64,
    pub length: u64,
    pub kind: String,
    pub parameters: String,
}

pub(super) fn put_string(target: &mut [u8], value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() >= target.len()
        || !value.bytes().all(|c| c.is_ascii_graphic())
    {
        return Err(io::Errno::INVAL);
    }
    target.fill(0);
    target[..value.len()].copy_from_slice(value.as_bytes());
    Ok(())
}
pub(super) fn dm_string(bytes: &[u8]) -> io::Result<&str> {
    let end = bytes.iter().position(|b| *b == 0).ok_or(io::Errno::PROTO)?;
    if end == 0 || !bytes[..end].iter().all(u8::is_ascii_graphic) {
        return Err(io::Errno::PROTO);
    }
    std::str::from_utf8(&bytes[..end]).map_err(|_| io::Errno::PROTO)
}
impl DmBuffer {
    pub(super) fn request(
        size: usize,
        device: u64,
        uuid: Option<&str>,
        table: bool,
    ) -> io::Result<Self> {
        if !(312..=DM_CAPACITY).contains(&size) || (device == 0) == uuid.is_none() {
            return Err(io::Errno::INVAL);
        }
        let mut buffer = Self {
            header: DmHeader {
                // These three operations are defined by DM ABI v4.0, used by
                // all current board kernels (including the selected 5.15 BSP).
                // No newer feature or inactive/deferred operation is requested.
                version: [4, 0, 0],
                data_size: size as u32,
                data_start: 312,
                target_count: 0,
                open_count: 0,
                flags: if table { DM_TABLE } else { 0 },
                event_nr: 0,
                padding: 0,
                dev: device,
                name: [0; 128],
                uuid: [0; 129],
                data: [0; 7],
            },
            data: [0; DM_CAPACITY - 312],
        };
        if let Some(uuid) = uuid {
            put_string(&mut buffer.header.uuid, uuid)?;
        }
        Ok(buffer)
    }
}
pub(super) fn valid_header(buffer: &DmBuffer, capacity: usize) -> io::Result<()> {
    let h = &buffer.header;
    if h.version[0] != 4
        || !(305..=capacity).contains(&(h.data_size as usize))
        || h.data_start != 312
        || h.padding != 0
    {
        return Err(io::Errno::PROTO);
    }
    Ok(())
}
pub(super) fn parse_dm_status(
    buffer: &DmBuffer,
    capacity: usize,
    table: bool,
) -> io::Result<DmStatus> {
    valid_header(buffer, capacity)?;
    let h = &buffer.header;
    let required = DM_READONLY | DM_ACTIVE | if table { DM_TABLE } else { 0 };
    let allowed = required | if table { DM_FULL } else { 0 };
    if h.flags & required != required
        || h.flags & !allowed != 0
        || !(1..=16).contains(&h.target_count)
        || h.open_count < 0
        || (!table && h.data_size != 305)
        || h.dev == 0
    {
        return Err(io::Errno::PROTO);
    }
    Ok(DmStatus {
        device: h.dev,
        name: dm_string(&h.name)?.into(),
        uuid: dm_string(&h.uuid)?.into(),
        targets: h.target_count,
        open_count: h.open_count,
        event: h.event_nr,
    })
}
pub(super) fn parse_dm_table(
    buffer: &DmBuffer,
    capacity: usize,
    expected: &DmStatus,
) -> io::Result<Vec<DmTarget>> {
    if &parse_dm_status(buffer, capacity, true)? != expected || buffer.header.flags & DM_FULL != 0 {
        return Err(io::Errno::PROTO);
    }
    let end = (buffer.header.data_size as usize)
        .checked_sub(312)
        .ok_or(io::Errno::PROTO)?;
    let data = buffer.data.get(..end).ok_or(io::Errno::PROTO)?;
    let mut at: usize = 0;
    let mut sector_end = 0_u64;
    let mut targets = Vec::new();
    for number in 0..expected.targets {
        let raw = data
            .get(at..at.checked_add(40).ok_or(io::Errno::PROTO)?)
            .ok_or(io::Errno::PROTO)?;
        let sector = u64::from_ne_bytes(raw[0..8].try_into().map_err(|_| io::Errno::PROTO)?);
        let length = u64::from_ne_bytes(raw[8..16].try_into().map_err(|_| io::Errno::PROTO)?);
        let status = i32::from_ne_bytes(raw[16..20].try_into().map_err(|_| io::Errno::PROTO)?);
        let next =
            u32::from_ne_bytes(raw[20..24].try_into().map_err(|_| io::Errno::PROTO)?) as usize;
        let last = number + 1 == expected.targets;
        // TABLE_STATUS next is relative to the FIRST spec, even for later
        // entries. Its final value includes alignment beyond data_size.
        if sector != sector_end
            || length == 0
            || status != 0
            || next <= at + 40
            || !next.is_multiple_of(8)
            || next > capacity - 312
            || (!last && next >= end)
            || (last && next != end.next_multiple_of(8))
        {
            return Err(io::Errno::PROTO);
        }
        sector_end = sector.checked_add(length).ok_or(io::Errno::PROTO)?;
        let params = data
            .get(at + 40..if last { end } else { next })
            .ok_or(io::Errno::PROTO)?;
        let nul = params
            .iter()
            .position(|b| *b == 0)
            .ok_or(io::Errno::PROTO)?;
        if params[..nul]
            .iter()
            .any(|c| !c.is_ascii_graphic() && *c != b' ')
            || (last && at + 40 + nul + 1 != end)
            || (at + 40 + nul + 1).next_multiple_of(8) != next
        {
            return Err(io::Errno::PROTO);
        }
        targets.push(DmTarget {
            sector,
            length,
            kind: dm_string(&raw[24..40])?.into(),
            parameters: std::str::from_utf8(&params[..nul])
                .map_err(|_| io::Errno::PROTO)?
                .into(),
        });
        at = next;
    }
    Ok(targets)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DmCommand {
    Status,
    Table,
    Remove,
    Create,
    Load,
    Resume,
}

#[allow(unsafe_code)]
pub(super) fn dm_exchange(
    fd: impl AsFd,
    command: DmCommand,
    buffer: &mut DmBuffer,
) -> io::Result<()> {
    // SAFETY: These fixed Linux DM requests encode the repr(C) 312-byte
    // header, followed by at most data_size initialized bytes. The full 65536
    // byte repr(C), 8-aligned object is live and exclusively borrowed throughout
    // ioctl; data_size is constructed internally and never exceeds that object.
    // Every kernel-writable field is an integer/byte array accepting all bit
    // patterns. Updater exposes no pointer outside this call; AsFd retains the
    // verified control descriptor. No user-selected request number is accepted.
    unsafe {
        match command {
            DmCommand::Status => ioctl::ioctl(fd, Updater::<DM_STATUS_REQUEST, _>::new(buffer)),
            DmCommand::Table => ioctl::ioctl(fd, Updater::<DM_TABLE_REQUEST, _>::new(buffer)),
            DmCommand::Remove => ioctl::ioctl(fd, Updater::<DM_REMOVE_REQUEST, _>::new(buffer)),
            DmCommand::Create => ioctl::ioctl(
                fd,
                Updater::<{ startup::DM_CREATE_REQUEST }, _>::new(buffer),
            ),
            DmCommand::Load => {
                ioctl::ioctl(fd, Updater::<{ startup::DM_LOAD_REQUEST }, _>::new(buffer))
            }
            DmCommand::Resume => ioctl::ioctl(
                fd,
                Updater::<{ startup::DM_RESUME_REQUEST }, _>::new(buffer),
            ),
        }
    }
}
pub(super) fn read_dm_status(
    device: u64,
    exchange: &mut impl FnMut(DmCommand, &mut DmBuffer) -> io::Result<()>,
) -> io::Result<DmStatus> {
    for _ in 0..4 {
        let mut buffer = DmBuffer::request(312, device, None, false)?;
        match exchange(DmCommand::Status, &mut buffer) {
            Err(io::Errno::INTR) => continue,
            result => result?,
        }
        let status = parse_dm_status(&buffer, 312, false)?;
        if status.device != device {
            return Err(io::Errno::PROTO);
        }
        return Ok(status);
    }
    Err(io::Errno::INTR)
}
pub(super) fn read_dm_table(
    expected: &DmStatus,
    exchange: &mut impl FnMut(DmCommand, &mut DmBuffer) -> io::Result<()>,
) -> io::Result<Vec<DmTarget>> {
    let mut capacity = 1024;
    let mut failure = io::Errno::OVERFLOW;
    for _ in 0..4 {
        let mut buffer = DmBuffer::request(capacity, 0, Some(&expected.uuid), true)?;
        match exchange(DmCommand::Table, &mut buffer) {
            Err(io::Errno::INTR) => {
                failure = io::Errno::INTR;
                continue;
            }
            result => result?,
        }
        failure = io::Errno::OVERFLOW;
        if &parse_dm_status(&buffer, capacity, true)? != expected {
            return Err(io::Errno::PROTO);
        }
        if buffer.header.flags & DM_FULL == 0 {
            return parse_dm_table(&buffer, capacity, expected);
        }
        capacity = (capacity * 4).min(DM_CAPACITY);
    }
    Err(failure)
}
pub(super) fn remove_dm(
    expected: &DmStatus,
    exchange: &mut impl FnMut(DmCommand, &mut DmBuffer) -> io::Result<()>,
) -> io::Result<()> {
    let current = read_dm_status(expected.device, exchange)?;
    if current.open_count != 0 {
        return Err(io::Errno::BUSY);
    }
    if &current != expected {
        return Err(io::Errno::PROTO);
    }
    let mut buffer = DmBuffer::request(312, 0, Some(&expected.uuid), false)?;
    // Never blindly retry a mutating ioctl, including EINTR: the supervisor
    // reobserves the complete live graph even when an operation returned error.
    exchange(DmCommand::Remove, &mut buffer)?;
    valid_header(&buffer, 312)?;
    let h = &buffer.header;
    // dev_remove fills name/uuid but does not call __dev_status. dev remains
    // zero for a UUID-selected request; it is not a post-removal device proof.
    if h.data_size != 305
        || h.dev != 0
        || h.target_count != 0
        || h.open_count != 0
        || h.flags & !DM_UEVENT != 0
        || dm_string(&h.name)? != expected.name
        || dm_string(&h.uuid)? != expected.uuid
    {
        return Err(io::Errno::PROTO);
    }
    Ok(())
}

/// Inspect active read-only DM state using its current encoded device identity.
pub fn dm_status(fd: impl AsFd, device: u64) -> io::Result<DmStatus> {
    read_dm_status(device, &mut |command, buffer| {
        dm_exchange(fd.as_fd(), command, buffer)
    })
}
/// Read the complete table, refusing identity/event/count races or partial output.
pub fn dm_table(fd: impl AsFd, expected: &DmStatus) -> io::Result<Vec<DmTarget>> {
    read_dm_table(expected, &mut |command, buffer| {
        dm_exchange(fd.as_fd(), command, buffer)
    })
}
/// Remove exactly the revalidated UUID on the same control FD, without force,
/// defer, cookies or blind retries. Caller must separately prove disappearance.
pub fn dm_remove(fd: impl AsFd, expected: &DmStatus) -> io::Result<()> {
    remove_dm(expected, &mut |command, buffer| {
        dm_exchange(fd.as_fd(), command, buffer)
    })
}

#[cfg(test)]
mod tests;
