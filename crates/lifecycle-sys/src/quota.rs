//! ext4 project quotas: a directory's project id, and a project's limits and
//! usage, without Debian's quota tools.

use rustix::{
    fd::{AsFd, AsRawFd},
    io,
    ioctl::{self, Setter, Updater},
};

/// `struct fsxattr` of `<linux/fs.h>`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct FsXattr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    projid: u32,
    cowextsize: u32,
    pad: [u8; 8],
}

/// `struct if_dqblk` of `<linux/quota.h>`: limits in 1 KiB blocks, space in
/// bytes.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct DqBlk {
    bhardlimit: u64,
    bsoftlimit: u64,
    curspace: u64,
    ihardlimit: u64,
    isoftlimit: u64,
    curinodes: u64,
    btime: u64,
    itime: u64,
    valid: u32,
}

const FS_GET_XATTR: u32 = 0x801c581f;
const FS_SET_XATTR: u32 = 0x401c5820;
const FS_XFLAG_PROJINHERIT: u32 = 0x200;
/// `QCMD(Q_GETQUOTA, PRJQUOTA)` and `QCMD(Q_SETQUOTA, PRJQUOTA)`.
const GET_PROJECT_QUOTA: u32 = (0x800007 << 8) | 2;
const SET_PROJECT_QUOTA: u32 = (0x800008 << 8) | 2;
/// `QIF_BLIMITS | QIF_ILIMITS`.
const QIF_LIMITS: u32 = 1 | 4;

const _: () = {
    assert!(size_of::<FsXattr>() == 28 && align_of::<FsXattr>() == 4);
    assert!(std::mem::offset_of!(FsXattr, projid) == 12);
    assert!(size_of::<DqBlk>() == 72 && align_of::<DqBlk>() == 8);
    assert!(std::mem::offset_of!(DqBlk, curspace) == 16);
    assert!(std::mem::offset_of!(DqBlk, curinodes) == 40);
    assert!(std::mem::offset_of!(DqBlk, valid) == 64);
    assert!(FS_GET_XATTR == ioctl::opcode::read::<FsXattr>(b'X', 31));
    assert!(FS_SET_XATTR == ioctl::opcode::write::<FsXattr>(b'X', 32));
    assert!(libc::SYS_quotactl_fd == 443);
};

/// A project's hard limits and usage. A limit of 0 is no limit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProjectQuota {
    pub block_limit_kib: u64,
    pub inode_limit: u64,
    pub used_bytes: u64,
    pub used_inodes: u64,
}

#[allow(unsafe_code)]
fn get_xattr(dir: impl AsFd) -> io::Result<FsXattr> {
    let mut value = FsXattr::default();
    // SAFETY: FS_IOC_FSGETXATTR writes exactly the initialized repr(C) 28-byte
    // fsxattr; every field accepts all bit patterns. Updater holds the only
    // borrow and the fd outlives the synchronous call.
    unsafe {
        ioctl::ioctl(dir, Updater::<FS_GET_XATTR, _>::new(&mut value))?;
    }
    Ok(value)
}

/// The project id of `dir`, and whether new entries under it inherit it.
pub fn project_of(dir: impl AsFd) -> io::Result<(u32, bool)> {
    let value = get_xattr(dir)?;
    Ok((value.projid, value.xflags & FS_XFLAG_PROJINHERIT != 0))
}

/// Give `dir` project `id` and have new entries under it inherit it, what
/// `chattr -p <id> +P` does. Entries already inside keep theirs.
#[allow(unsafe_code)]
pub fn set_project_inherit(dir: impl AsFd, id: u32) -> io::Result<()> {
    let mut value = get_xattr(&dir)?;
    value.projid = id;
    value.xflags |= FS_XFLAG_PROJINHERIT;
    // SAFETY: FS_IOC_FSSETXATTR only reads the 28-byte repr(C) fsxattr the
    // Setter owns for the synchronous call; the fd outlives it.
    unsafe { ioctl::ioctl(&dir, Setter::<FS_SET_XATTR, _>::new(value)) }
}

#[allow(unsafe_code)]
fn quotactl(fs: impl AsFd, command: u32, id: u32, value: &mut DqBlk) -> io::Result<()> {
    // SAFETY: quotactl_fd(2) takes a live descriptor (borrowed for the call),
    // a fixed command, an id and a pointer to one initialized repr(C)
    // if_dqblk, which the kernel reads (Q_SETQUOTA) or writes (Q_GETQUOTA)
    // before returning. The exclusive borrow covers both.
    let result = unsafe {
        libc::syscall(
            libc::SYS_quotactl_fd,
            fs.as_fd().as_raw_fd(),
            command,
            id,
            std::ptr::from_mut(value),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))
    }
}

/// Project `id`'s limits and usage on the filesystem `fs` is open on.
pub fn project_quota(fs: impl AsFd, id: u32) -> io::Result<ProjectQuota> {
    let mut value = DqBlk::default();
    quotactl(fs, GET_PROJECT_QUOTA, id, &mut value)?;
    Ok(ProjectQuota {
        block_limit_kib: value.bhardlimit,
        inode_limit: value.ihardlimit,
        used_bytes: value.curspace,
        used_inodes: value.curinodes,
    })
}

/// Set project `id`'s hard limits on the filesystem `fs` is open on, soft
/// limits none, what `setquota -P <id> 0 <kib> 0 <inodes>` does.
pub fn set_project_limits(fs: impl AsFd, id: u32, block_kib: u64, inodes: u64) -> io::Result<()> {
    let mut value = DqBlk {
        bhardlimit: block_kib,
        ihardlimit: inodes,
        valid: QIF_LIMITS,
        ..DqBlk::default()
    };
    quotactl(fs, SET_PROJECT_QUOTA, id, &mut value)
}
