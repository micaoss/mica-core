//! The shutdown's access to the running system.

use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use super::*;

pub struct SystemIo<'a> {
    pub supervisor: &'a mut Supervisor,
    pub executable: &'static str,
}
impl SystemIo<'_> {
    pub(super) fn worker(&mut self, op: &Operation, deadline: u64) -> Result<Vec<u8>> {
        let argument = serde_json::to_string(op)?;
        ensure!(argument.len() <= 16384, "excessive worker request");
        let mut command = Command::new(self.executable);
        command.args(["--lifecycle-worker", &argument]);
        self.supervisor.run(command, deadline, 1024 * 1024)
    }
}
impl LifecycleIo for SystemIo<'_> {
    fn now_ms(&self) -> u64 {
        self.supervisor.now_ms()
    }
    fn scan(&mut self, deadline: u64) -> Result<Snapshot> {
        Ok(serde_json::from_slice(
            &self.worker(&Operation::Scan, deadline)?,
        )?)
    }
    fn execute(&mut self, op: &Operation, deadline: u64) -> Result<()> {
        self.worker(op, deadline)?;
        Ok(())
    }
    fn event(&mut self, stage: &str, detail: &str) {
        if diagnostic(&format!("MICA_SHUTDOWN stage={stage} detail={detail:?}")).is_err() {
            self.supervisor.poisoned = true;
        }
    }
    fn terminal(&mut self, action: Action, released: Released) -> Result<()> {
        self.supervisor.terminal(action, released)
    }
}

pub(super) fn api(name: &str, magic: i64) -> Result<PathBuf> {
    for base in [Path::new("/"), Path::new("/newroot")] {
        let path = base.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
            && rustix::fs::statfs(&path).is_ok_and(|s| s.f_type == magic)
        {
            return Ok(path);
        }
    }
    bail!("required {name} filesystem unavailable")
}
pub(super) fn procfs() -> Result<PathBuf> {
    api("proc", 0x9fa0)
}
pub(super) fn sysfs() -> Result<PathBuf> {
    api("sys", 0x62656572)
}
pub(super) fn devfs() -> Result<PathBuf> {
    api("dev", 0x01021994)
}

pub(super) fn dm_control() -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(
            (rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW)
                .bits() as i32,
        )
        .open(devfs()?.join("mapper/control"))?;
    let expected =
        Device::parse(read_text(sysfs()?.join("class/misc/device-mapper/dev"), 64)?.trim())?;
    ensure!(
        file.metadata()?.file_type().is_char_device()
            && Device::from_raw(file.metadata()?.rdev()) == expected,
        "device-mapper control descriptor identity mismatch"
    );
    Ok(file)
}

pub(super) fn checked_verity_table(
    status: &lifecycle_sys::DmStatus,
    targets: &[lifecycle_sys::DmTarget],
    device: Device,
    name: &str,
    uuid: &str,
    slaves: &BTreeSet<Device>,
    sectors: u64,
) -> Result<String> {
    ensure!(
        Device::from_raw(status.device) == device
            && status.name == name
            && status.uuid == uuid
            && !uuid.is_empty()
            && status.targets == 1
            && targets.len() == 1,
        "DM device/name/UUID/target identity mismatch"
    );
    let target = &targets[0];
    ensure!(
        target.kind == "verity" && target.sector == 0 && target.length == sectors && sectors > 0,
        "DM table does not completely cover the verified device"
    );
    let parameters: Vec<_> = target.parameters.split_whitespace().collect();
    ensure!(
        parameters.len() >= 10 && parameters[0] == "1",
        "unsupported MICA verity table"
    );
    let providers = BTreeSet::from([Device::parse(parameters[1])?, Device::parse(parameters[2])?]);
    ensure!(
        providers == *slaves && providers.len() == 1 && providers.iter().all(|d| d.major == 7),
        "DM table providers differ from live MICA loop dependencies"
    );
    Ok(format!(
        "{} {} {} {}",
        target.sector,
        target.length,
        target.kind,
        parameters.join(" ")
    ))
}

pub(super) fn read_mica_table(
    control: &File,
    device: Device,
    path: &Path,
    name: &str,
    uuid: &str,
    slaves: &BTreeSet<Device>,
) -> Result<(String, lifecycle_sys::DmStatus)> {
    let status =
        lifecycle_sys::dm_status(control, rustix::fs::makedev(device.major, device.minor))?;
    let targets = lifecycle_sys::dm_table(control, &status)?;
    let sectors = read_text(path.join("size"), 64)?.trim().parse::<u64>()?;
    let table = checked_verity_table(&status, &targets, device, name, uuid, slaves, sectors)?;
    ensure!(
        lifecycle_sys::dm_status(control, status.device)? == status,
        "DM identity changed while reading its table"
    );
    Ok((table, status))
}
pub(super) fn list(path: &Path, limit: usize) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        ensure!(paths.len() < limit, "excessive kernel directory");
        paths.push(entry?.path());
    }
    paths.sort();
    Ok(paths)
}
pub(super) fn links(path: &Path) -> Result<BTreeSet<Device>> {
    list(path, 4096)?
        .into_iter()
        .map(|p| Device::parse(read_text(p.join("dev"), 64)?.trim()))
        .collect()
}
pub(super) fn slave_links(path: &Path) -> Result<BTreeSet<Device>> {
    if path.join("partition").exists() {
        ensure!(
            read_text(path.join("partition"), 64)?
                .trim()
                .parse::<u32>()?
                > 0,
            "invalid partition identity"
        );
        // Linux creates holders for partitions, but only whole disks have a
        // slaves directory. Absence elsewhere remains an observation error.
        return Ok(BTreeSet::new());
    }
    links(&path.join("slaves"))
}
pub(super) fn disk_generation(path: &Path) -> Result<u64> {
    let node = fs::canonicalize(path)?;
    let disk = if node.join("partition").exists() {
        node.parent().context("partition has no disk")?
    } else {
        node.as_path()
    };
    let generation = read_text(disk.join("diskseq"), 64)?.trim().parse()?;
    ensure!(generation > 0, "invalid block generation");
    Ok(generation)
}
pub(super) fn inspect_loop_at(
    path: &Path,
    device: Device,
    generation: u64,
) -> Result<Option<LoopIdentity>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW)
                .bits() as i32,
        )
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.file_type().is_block_device() && Device::from_raw(metadata.rdev()) == device,
        "loop descriptor identity mismatch"
    );
    let state = lifecycle_sys::loop_status(&file)?;
    // Drop the inspection FD before returning an observation of association state.
    drop(file);
    state
        .map(|s| {
            ensure!(
                s.number == device.minor && device.major == 7,
                "loop number mismatch"
            );
            Ok(LoopIdentity {
                device,
                generation,
                backing: Device::from_raw(s.backing_device),
                inode: s.inode,
                offset: s.offset,
                size_limit: s.size_limit,
                flags: s.flags,
            })
        })
        .transpose()
}
pub(super) fn process_ids(proc: &Path) -> Result<Vec<u32>> {
    let own = std::process::id();
    let mut users = Vec::new();
    for path in list(proc, 16384)? {
        let Some(pid) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == 1 || pid == own {
            continue;
        }
        let stat = match read_text(path.join("stat"), 8192) {
            Ok(stat) => stat,
            Err(_) if !path.exists() => continue,
            Err(error) => return Err(error),
        };
        let (_, rest) = stat.rsplit_once(") ").context("malformed process stat")?;
        let fields: Vec<_> = rest.split_whitespace().collect();
        ensure!(fields.len() > 19, "short process stat");
        let flags = fields[6].parse::<u64>()?;
        if flags & 0x0020_0000 == 0 && fields[0] != "Z" {
            users.push(pid);
        }
    }
    ensure!(users.len() <= 4096, "excessive userspace holders");
    Ok(users)
}
pub(super) fn snapshot() -> Result<Snapshot> {
    let proc = procfs()?;
    let sys = sysfs()?;
    let dev = devfs()?;
    let mounts = parse_mountinfo(&read_text(proc.join("self/mountinfo"), 1024 * 1024)?)?;
    let mut blocks = Vec::new();
    for path in list(&sys.join("class/block"), 4096)? {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid block name")?
            .to_owned();
        let device = Device::parse(read_text(path.join("dev"), 64)?.trim())?;
        let mut generation = disk_generation(&path)?;
        let mut association = None;
        if device.major == 7 {
            let mut stable = false;
            for _ in 0..3 {
                let observed = inspect_loop_at(&dev.join(&name), device, generation)?;
                let after = disk_generation(&path)?;
                if generation == after {
                    association = observed;
                    stable = true;
                    break;
                }
                generation = after;
            }
            ensure!(stable, "loop changed during descriptor inspection");
        }
        let holders = links(&path.join("holders"))?;
        let slaves = slave_links(&path)?;
        let mapping = if path.join("dm").is_dir() {
            let dm_name = read_text(path.join("dm/name"), 256)?.trim().to_owned();
            let uuid = read_text(path.join("dm/uuid"), 256)?.trim().to_owned();
            // Only MICA tables need content identity. Foreign holders remain
            // visible but are never passed to a removal command.
            let table = if crate::boot::mica_mapping(&dm_name) {
                let control = dm_control()?;
                let (table, _) =
                    read_mica_table(&control, device, &path, &dm_name, &uuid, &slaves)?;
                drop(control);
                ensure!(
                    disk_generation(&path)? == generation
                        && read_text(path.join("dm/name"), 256)?.trim() == dm_name
                        && read_text(path.join("dm/uuid"), 256)?.trim() == uuid,
                    "DM generation or identity changed during inspection"
                );
                table
            } else {
                String::new()
            };
            Some(Mapping {
                device,
                generation,
                name: dm_name,
                uuid,
                table,
            })
        } else {
            None
        };
        blocks.push(Block {
            device,
            generation,
            name,
            holders,
            slaves,
            association,
            mapping,
        });
    }
    let swap_text = read_text(proc.join("swaps"), 65536)?;
    ensure!(
        swap_text
            .lines()
            .next()
            .is_some_and(|s| s.starts_with("Filename")),
        "invalid swap state"
    );
    let swaps = swap_text.lines().skip(1).map(str::to_owned).collect();
    refresh_vm_stats(&proc);
    let dirty = dirty_kib(&read_text(proc.join("meminfo"), 16384)?)?;
    Ok(Snapshot {
        mounts,
        blocks,
        processes: process_ids(&proc)?,
        swaps,
        dirty_kib: dirty,
    })
}
/// Fold every CPU's pending vm statistics into the global counters
/// (`/proc/sys/vm/stat_refresh`), so the dirty count read next is exact.
///
/// `meminfo` reads the global counters alone, and a CPU can hold a page's
/// worth of accounting there for as long as it stays idle: a sync never
/// changes that, and a count that must reach zero would wait on it for good.
/// A root without the file keeps the raw count, which only errs towards
/// refusing to release. The file is the kernel's; it is never created here.
pub(super) fn refresh_vm_stats(proc: &Path) {
    use std::io::Write as _;
    if let Ok(mut file) = OpenOptions::new()
        .write(true)
        .open(proc.join("sys/vm/stat_refresh"))
    {
        let _ = file.write_all(b"1\n");
    }
}

/// KiB of page cache still to be written: `Dirty`, `Writeback` and
/// `NFS_Unstable` of `meminfo`, all three required.
pub(super) fn dirty_kib(meminfo: &str) -> Result<u64> {
    let mut dirty = 0_u64;
    let mut fields = 0;
    for line in meminfo.lines() {
        if ["Dirty:", "Writeback:", "NFS_Unstable:"]
            .iter()
            .any(|p| line.starts_with(p))
        {
            let value = line
                .split_whitespace()
                .nth(1)
                .context("invalid dirty state")?
                .parse::<u64>()?;
            dirty = dirty.checked_add(value).context("dirty count overflow")?;
            fields += 1;
        }
    }
    ensure!(fields == 3, "missing dirty state");
    Ok(dirty)
}

pub(super) fn current_mount(expected: &Mount) -> Result<()> {
    let mounts = parse_mountinfo(&read_text(procfs()?.join("self/mountinfo"), 1024 * 1024)?)?;
    ensure!(
        mounts.iter().any(|m| m == expected),
        "mount identity changed before operation"
    );
    let stat = rustix::fs::statx(
        rustix::fs::CWD,
        &expected.path,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW | rustix::fs::AtFlags::NO_AUTOMOUNT,
        rustix::fs::StatxFlags::MNT_ID,
    )?;
    ensure!(
        stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() != 0
            && stat.stx_mnt_id == expected.id,
        "mount is hidden or was replaced"
    );
    Ok(())
}
pub(super) fn quiesce() -> Result<()> {
    let proc = procfs()?;
    let mut handles = Vec::new();
    for pid in process_ids(&proc)? {
        let id = rustix::process::Pid::from_raw(pid as i32).context("invalid process id")?;
        let before = read_text(proc.join(format!("{pid}/stat")), 8192)?;
        let handle = match rustix::process::pidfd_open(id, rustix::process::PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::SRCH) => continue,
            Err(error) => return Err(error.into()),
        };
        let after = read_text(proc.join(format!("{pid}/stat")), 8192)?;
        let birth = |s: &str| {
            s.rsplit_once(") ")
                .and_then(|(_, tail)| tail.split_whitespace().nth(19))
                .map(str::to_owned)
        };
        ensure!(
            birth(&before).is_some() && birth(&before) == birth(&after),
            "process reused during quiesce"
        );
        let _ = rustix::process::pidfd_send_signal(&handle, rustix::process::Signal::TERM);
        handles.push(handle);
    }
    if !handles.is_empty() {
        thread::sleep(Duration::from_millis(250));
    }
    for handle in handles {
        match rustix::process::pidfd_send_signal(&handle, rustix::process::Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
