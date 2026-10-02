//! The privileged worker that performs one operation.

use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

use super::*;

/// Fixed child protocol, reachable only as a direct child of lifecycle PID 1.
/// It carries typed operations and no command, environment or path-root override.
pub fn worker(args: &[String]) -> Option<Result<()>> {
    if args.first().map(String::as_str) != Some("--lifecycle-worker") {
        return None;
    }
    Some((|| {
        ensure!(
            rustix::process::getppid() == rustix::process::Pid::from_raw(1),
            "lifecycle worker requires PID1 parent"
        );
        ensure!(
            args.len() == 2 && args[1].len() <= 16384,
            "invalid worker input"
        );
        let op: Operation = serde_json::from_str(&args[1])?;
        perform(&op)
    })())
}

pub(super) fn perform(op: &Operation) -> Result<()> {
    match op {
        Operation::Scan => {
            println!("{}", serde_json::to_string(&snapshot()?)?);
        }
        Operation::Private => {
            // Privatize before restoring moved APIs: moving beneath a shared
            // parent is rejected by mount(2). Inherited stdin also works while
            // /dev/null temporarily lives below /newroot/dev.
            rustix::mount::mount_change(
                "/",
                rustix::mount::MountPropagationFlags::PRIVATE
                    | rustix::mount::MountPropagationFlags::REC,
            )?;
            // Startup can fail between API mount moves. Restore them before
            // traversing/removing the old root; validate the real fs types.
            for (name, magic) in [("dev", 0x01021994), ("proc", 0x9fa0), ("sys", 0x62656572)] {
                let source = api(name, magic)?;
                let target = Path::new("/").join(name);
                if source != target {
                    rustix::mount::mount_move(&source, &target)?;
                }
            }
        }
        Operation::Quiesce => quiesce()?,
        Operation::Sync => rustix::fs::sync(),
        Operation::SyncMount(mount) => {
            current_mount(mount)?;
            let fd = rustix::fs::open(
                &mount.path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            let identity = rustix::fs::statx(
                &fd,
                "",
                rustix::fs::AtFlags::EMPTY_PATH,
                rustix::fs::StatxFlags::MNT_ID,
            )?;
            ensure!(
                identity.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() != 0
                    && identity.stx_mnt_id == mount.id,
                "sync mount descriptor changed"
            );
            // sync cannot report writeback errors. Per-filesystem syncfs
            // reports EIO/ENOSPC before ordinary unmount releases this mount.
            rustix::fs::syncfs(&fd)?;
            drop(fd);
        }
        Operation::Unmount(mount) => {
            current_mount(mount)?;
            rustix::mount::unmount(&mount.path, rustix::mount::UnmountFlags::empty())?;
        }
        Operation::MoveBacking(mount) => {
            current_mount(mount)?;
            let target = format!("/backing/{}", mount.id);
            let root = rustix::fs::open(
                "/",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
            )?;
            match rustix::fs::mkdirat(&root, "backing", rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            let directory = rustix::fs::openat2(
                &root,
                "backing",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
                rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS,
            )?;
            match rustix::fs::mkdirat(
                &directory,
                mount.id.to_string(),
                rustix::fs::Mode::from_raw_mode(0o700),
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            let target_fd = rustix::fs::openat2(
                &directory,
                mount.id.to_string(),
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                rustix::fs::Mode::empty(),
                rustix::fs::ResolveFlags::BENEATH
                    | rustix::fs::ResolveFlags::NO_SYMLINKS
                    | rustix::fs::ResolveFlags::NO_XDEV,
            )?;
            ensure!(
                rustix::fs::fstat(&target_fd)?.st_dev == rustix::fs::fstat(&root)?.st_dev
                    && fs::read_dir(&target)?.next().is_none(),
                "backing destination is not empty exitrd storage"
            );
            drop(target_fd);
            drop(directory);
            drop(root);
            rustix::mount::mount_move(&mount.path, &target)?;
            let moved =
                parse_mountinfo(&read_text(procfs()?.join("self/mountinfo"), 1024 * 1024)?)?;
            ensure!(
                moved.iter().any(|m| m.id == mount.id
                    && m.device == mount.device
                    && m.root == mount.root
                    && m.path == target),
                "moved mount identity mismatch"
            );
        }
        Operation::RemoveMapping(expected) => {
            ensure!(
                crate::boot::mica_mapping(&expected.name),
                "foreign mapping removal refused"
            );
            let state = snapshot()?;
            let block = state
                .blocks
                .iter()
                .find(|b| b.device == expected.device)
                .context("mapping disappeared before removal")?;
            ensure!(
                block.mapping.as_ref() == Some(expected)
                    && block.holders.is_empty()
                    && !state.mounts.iter().any(|m| m.device == expected.device),
                "mapping identity or users changed"
            );
            let path = sysfs()?.join("class/block").join(&block.name);
            let control = dm_control()?;
            let (table, status) = read_mica_table(
                &control,
                expected.device,
                &path,
                &expected.name,
                &expected.uuid,
                &block.slaves,
            )?;
            ensure!(
                table == expected.table && disk_generation(&path)? == expected.generation,
                "DM table/generation changed before removal"
            );
            lifecycle_sys::dm_remove(&control, &status)?;
            drop(control);
            let after = snapshot()?;
            ensure!(
                !after.blocks.iter().any(|b| b.device == expected.device
                    || b.mapping
                        .as_ref()
                        .is_some_and(|m| m.name == expected.name || m.uuid == expected.uuid)),
                "DM removal did not release the observed device"
            );
        }
        Operation::DetachLoop(expected) => {
            let state = snapshot()?;
            let block = state
                .blocks
                .iter()
                .find(|b| b.device == expected.device)
                .context("loop missing before detach")?;
            ensure!(
                block.holders.is_empty()
                    && !state.mounts.iter().any(|m| m.device == expected.device),
                "loop still has users"
            );
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(
                    (rustix::fs::OFlags::CLOEXEC
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::NOFOLLOW)
                        .bits() as i32,
                )
                .open(devfs()?.join(&block.name))?;
            ensure!(
                file.metadata()?.file_type().is_block_device()
                    && Device::from_raw(file.metadata()?.rdev()) == expected.device,
                "loop descriptor changed"
            );
            let current =
                lifecycle_sys::loop_status(&file)?.context("loop association disappeared")?;
            ensure!(
                current.number == expected.device.minor
                    && Device::from_raw(current.backing_device) == expected.backing
                    && current.inode == expected.inode
                    && current.offset == expected.offset
                    && current.size_limit == expected.size_limit
                    && current.flags & !4 == expected.flags & !4,
                "loop association changed before detach"
            );
            ensure!(
                disk_generation(&sysfs()?.join("class/block").join(&block.name))?
                    == expected.generation,
                "loop generation changed before detach"
            );
            lifecycle_sys::clear_loop(&file)?;
            drop(file);
            // The parent obtains a new snapshot after this worker exits. This
            // call is never itself reported as association/holder release.
        }
        Operation::Retire {
            id,
            board,
            system,
            system_device,
            boot_device,
        } => {
            ensure!(
                fs::read_link(procfs()?.join("1/exe"))? == Path::new("/init"),
                "record retirement requires startup PID1"
            );
            crate::deployments::valid_id(id)?;
            let backend = &board.boot;
            let name = Path::new(system_device)
                .file_name()
                .context("SYSTEM name")?;
            let node = fs::canonicalize(sysfs()?.join("class/block").join(name))?;
            let expected = crate::deployments::boot_partition(&node, board)?;
            ensure!(
                expected == Path::new(boot_device),
                "retirement boot device mismatch"
            );
            let system_dev = Device::from_raw(fs::metadata(system_device)?.rdev());
            let state = snapshot()?;
            ensure!(
                state.mounts.iter().any(|m| m.path == *system
                    && m.device == system_dev
                    && m.kind == "ext4"
                    && m.root == "/"),
                "retirement SYSTEM mount identity mismatch"
            );
            let boot = match backend {
                super::super::BootKind::Uefi => {
                    fs::create_dir_all("/boot-state")?;
                    rustix::mount::mount(
                        boot_device,
                        "/boot-state",
                        "vfat",
                        rustix::mount::MountFlags::NODEV
                            | rustix::mount::MountFlags::NOSUID
                            | rustix::mount::MountFlags::NOEXEC,
                        None,
                    )?;
                    crate::deployments::BootBackend::Uefi {
                        esp: "/boot-state".into(),
                    }
                }
                super::super::BootKind::UbootFit => crate::deployments::BootBackend::Fit {
                    firmware: expected,
                    layout: board
                        .records
                        .context("FIT boot records absent from the boot policy")?,
                },
            };
            let store = crate::deployments::DeploymentStore::new(
                system.into(),
                boot,
                "/unused-meta".into(),
            );
            let result = store.retire_failed_confirmed(id);
            if *backend == super::super::BootKind::Uefi {
                rustix::mount::mount_remount(
                    "/boot-state",
                    rustix::mount::MountFlags::RDONLY
                        | rustix::mount::MountFlags::NODEV
                        | rustix::mount::MountFlags::NOSUID
                        | rustix::mount::MountFlags::NOEXEC,
                    "",
                )?;
            }
            println!("{}", result?);
        }
    }
    Ok(())
}
