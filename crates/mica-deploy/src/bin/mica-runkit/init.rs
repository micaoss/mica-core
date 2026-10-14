//! The authenticated initramfs PID 1. No network or shell policy is accepted.

use anyhow::{Context, Result, ensure};
use mica_deploy::{
    board::BoardFacts,
    boot::{
        shutdown::{self, Action, Device, LifecycleIo, Operation, Ownership, Supervisor, SystemIo},
        startup::native,
    },
    components::BootIdentity,
    deployments::boot_partition,
};
use serde::Deserialize;
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

mod boot;
mod mounts;
use boot::*;
use mounts::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Config {
    identity: BootIdentity,
    board: BoardFacts,
    public_keys: Vec<String>,
    system_part_uuid: String,
    data_part_uuid: String,
}

#[derive(Debug)]
struct SharedSystemFailure;
impl std::fmt::Display for SharedSystemFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shared SYSTEM unavailable; full-image reflash recovery required")
    }
}
impl std::error::Error for SharedSystemFailure {}

struct BootAttempt {
    id: String,
    system_device: String,
    board: BoardFacts,
    /// Whether the boot composed a core set on trial. Its failure is the
    /// set's, whose trial has already paid for it.
    core_trial: bool,
}

struct BootControl {
    supervisor: Supervisor,
    storage: Ownership,
}

impl BootControl {
    fn observe(&mut self) -> Result<shutdown::Snapshot> {
        self.supervisor.observe("/sbin/mica-shutdown")
    }

    fn backing(&mut self, path: &str) -> Result<()> {
        let metadata = fs::metadata(path)?;
        ensure!(
            metadata.file_type().is_block_device(),
            "backing source is not a block device"
        );
        let device = Device::from_raw(metadata.rdev());
        let state = self.observe()?;
        let generation = state
            .blocks
            .iter()
            .find(|b| b.device == device)
            .context("backing block missing from live graph")?
            .generation;
        if let Some((_, known)) = self
            .storage
            .backing_generations
            .iter()
            .find(|(id, _)| *id == device)
        {
            ensure!(
                *known == generation,
                "backing block was reused during startup"
            );
        } else {
            self.storage.backing_generations.push((device, generation));
        }
        self.storage.backings.insert(device);
        Ok(())
    }
}

impl BootAttempt {
    fn retire_failed_confirmed(&self, control: &mut BootControl) -> Result<()> {
        let system = fs::canonicalize(
            Path::new("/sys/class/block").join(
                Path::new(&self.system_device)
                    .file_name()
                    .context("missing SYSTEM device")?,
            ),
        )?;
        let device = boot_partition(&system, &self.board)?;
        let boot_device = device.to_str().context("invalid boot device")?.to_owned();
        control.backing(&boot_device)?;
        let state = control.observe()?;
        let expected = Device::from_raw(fs::metadata(&self.system_device)?.rdev());
        let system_root = state
            .mounts
            .iter()
            .find(|m| m.device == expected && m.kind == "ext4" && m.root == "/")
            .context("verified SYSTEM mount no longer available for retirement")?
            .path
            .clone();
        let deadline = control
            .supervisor
            .begin_shutdown()?
            .operation_deadline(control.supervisor.now_ms())?;
        SystemIo {
            supervisor: &mut control.supervisor,
            executable: "/sbin/mica-shutdown",
        }
        .execute(
            &Operation::Retire {
                id: self.id.clone(),
                board: self.board.clone(),
                system: system_root,
                system_device: self.system_device.clone(),
                boot_device,
            },
            deadline,
        )
    }
}

pub fn main() {
    let _ = rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
    if let Some(result) = native::worker(&std::env::args().skip(1).collect::<Vec<_>>()) {
        if let Err(error) = result {
            eprintln!("mica-init startup worker: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if std::process::id() != 1 {
        eprintln!("mica-init must run as PID 1");
        std::process::exit(1);
    }
    let mut control = BootControl {
        supervisor: Supervisor::new(),
        storage: Ownership::default(),
    };
    let mut attempt = None;
    let error = boot(&mut control, &mut attempt)
        .err()
        .unwrap_or_else(|| anyhow::anyhow!("boot unexpectedly returned"));
    let _ = shutdown::diagnostic(&format!("mica-init: boot refused: {error:#}"));
    let mut recovery = error.is::<SharedSystemFailure>()
        || error.is::<mica_deploy::deployments::SharedDataFailure>();
    let cleanup = (|| -> Result<()> {
        let budget = control.supervisor.begin_shutdown()?;
        let deadline = budget.operation_deadline(control.supervisor.now_ms())?;
        SystemIo {
            supervisor: &mut control.supervisor,
            executable: "/sbin/mica-shutdown",
        }
        .execute(&Operation::Private, deadline)?;
        let deadline = budget.operation_deadline(control.supervisor.now_ms())?;
        SystemIo {
            supervisor: &mut control.supervisor,
            executable: "/sbin/mica-shutdown",
        }
        .execute(&Operation::Quiesce, deadline)?;
        ensure!(
            control.observe()?.processes.is_empty(),
            "startup workers remain before record retirement"
        );
        control.supervisor.prepare_process()?;
        if !recovery
            && let Err(error) = attempt
                .as_ref()
                .context("boot selection could not be established")
                .and_then(|selected| {
                    if selected.core_trial {
                        // The deployment is sound; the next boot spends
                        // another boot of the set's trial, or leaves it.
                        return shutdown::diagnostic(
                            "mica-init: the core set on trial failed; the deployment is kept",
                        );
                    }
                    selected.retire_failed_confirmed(&mut control)
                })
        {
            let _ = shutdown::diagnostic(&format!("mica-init: recovery required: {error:#}"));
            recovery = true;
        }
        let action = if recovery {
            Action::Poweroff
        } else {
            Action::Reboot
        };
        shutdown::diagnostic(&format!(
            "MICA_SHUTDOWN stage=entered action={} source=partial-startup",
            action.as_str()
        ))?;
        shutdown::finish(
            &mut SystemIo {
                supervisor: &mut control.supervisor,
                executable: "/sbin/mica-shutdown",
            },
            budget,
            &control.storage,
            action,
        )
    })();
    let failure = cleanup
        .err()
        .unwrap_or_else(|| anyhow::anyhow!("shutdown returned"));
    control.supervisor.failure(&failure)
}
