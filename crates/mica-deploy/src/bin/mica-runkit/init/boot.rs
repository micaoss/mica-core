//! The boot: selecting, verifying and switching to a deployment.

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    boot::{
        BootKind, copy_exitrd, data_projects, exitrd_tmpfs_bytes, fit_selected, openrc_root,
        persistent_machine_id, selected_entry, shutdown,
        startup::{
            self,
            native::{self, Operation as Startup},
        },
        utf16_variable,
    },
    components::{component_id, verify_deployment},
};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use super::*;

pub(super) fn boot(control: &mut BootControl, attempt: &mut Option<BootAttempt>) -> Result<()> {
    let started = Instant::now();
    ensure!(std::process::id() == 1, "mica-init must run as PID 1");
    mount(control, "devtmpfs", "/dev", "devtmpfs", "nosuid,mode=0755")?;
    mount(control, "proc", "/proc", "proc", "nosuid,nodev,noexec")?;
    mount(control, "sysfs", "/sys", "sysfs", "nosuid,nodev,noexec")?;
    mount(
        control,
        "tmpfs",
        "/run",
        "tmpfs",
        "nosuid,nodev,mode=0755,size=32M",
    )?;
    ensure!(
        fs::read_to_string("/sys/module/dm_verity/parameters/require_signatures")?.trim() == "Y",
        "signature enforcement is disabled"
    );
    // The policy is read first because it names the watchdog; it is a file of
    // the signed initramfs, and nothing of deployment storage is touched yet.
    let config: Config = serde_json::from_slice(&bounded_file("/etc/mica/boot.json", 4096)?)?;
    config.board.validate(&config.identity.arch)?;
    // Arming is synchronous and precedes access to deployment storage. The
    // signed cmdline fixes the timeout; NOWAYOUT keeps it armed across exec.
    let watchdog = shutdown::resolve_watchdog(
        Path::new("/sys/class/watchdog"),
        config.board.watchdog.as_ref().map(|w| w.identity.as_str()),
    )?;
    control.supervisor.arm(&watchdog)?;
    control.storage.watchdog = watchdog;
    eprintln!("mica-init: boot watchdog armed");
    eprintln!("mica-init: pseudo-filesystems and signature policy ready");
    for uuid in [&config.system_part_uuid, &config.data_part_uuid] {
        ensure!(
            uuid.len() == 36 && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
            "invalid storage UUID"
        );
    }
    ensure!(
        fs::read_to_string("/proc/sys/kernel/osrelease")?.trim() == config.identity.kernel_release,
        "running kernel release mismatch"
    );
    let keys = config
        .public_keys
        .iter()
        .map(|key| -> Result<[u8; 32]> {
            let raw = STANDARD.decode(key)?;
            ensure!(STANDARD.encode(&raw) == *key, "noncanonical metadata key");
            raw.try_into()
                .map_err(|_| anyhow::anyhow!("invalid metadata key length"))
        })
        .collect::<Result<Vec<_>>>()?;
    let backend = config.board.boot;
    let (selected, id, secure_boot) = match backend {
        BootKind::Uefi => {
            mount(
                control,
                "efivarfs",
                "/sys/firmware/efi/efivars",
                "efivarfs",
                "nosuid,nodev,noexec",
            )?;
            let selected = utf16_variable(&bounded_file(
                "/sys/firmware/efi/efivars/LoaderEntrySelected-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f",
                512,
            )?)?;
            let id = selected_entry(&selected)?;
            let secure_boot = fs::read(
                "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c",
            )?
            .get(4)
                == Some(&1);
            (selected, id, secure_boot)
        }
        BootKind::UbootFit => {
            let id = fit_selected(&bounded_file(
                "/sys/firmware/devicetree/base/chosen/mica,deployment-id",
                65,
            )?)?;
            (format!("fit:{id}"), id, false)
        }
    };
    eprintln!("mica-init: selected deployment {id}");
    control.storage.deployment = id.clone();
    let mut device = String::new();
    let discovery = Instant::now();
    while discovery.elapsed() < Duration::from_secs(15) {
        if let Ok(found) = run(
            control,
            Startup::Partition {
                uuid: config.system_part_uuid.clone(),
            },
        ) && !found.is_empty()
        {
            device = found;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !device.starts_with("/dev/") || device.contains(char::is_whitespace) {
        return Err(
            anyhow::anyhow!("SYSTEM partition not found uniquely").context(SharedSystemFailure)
        );
    }
    // ext4 replays a dirty journal before completing this read-only mount.
    mount(
        control,
        &device,
        "/system",
        "ext4",
        "ro,nodev,nosuid,noexec",
    )
    .context(SharedSystemFailure)?;
    *attempt = Some(BootAttempt {
        id: id.clone(),
        system_device: device.clone(),
        board: config.board.clone(),
    });
    eprintln!("mica-init: SYSTEM mounted read-only");
    let envelope = bounded_file(&format!("/system/deployments/{id}.json"), 24576)?;
    let deployment = verify_deployment(&envelope, &keys, &config.identity, config.board.kernel)?;
    let value = serde_json::to_value(&deployment)?;
    ensure!(
        component_id(&value)? == id,
        "selected deployment identity mismatch"
    );
    let paths = deployment.paths()?;
    verified_mount(
        control,
        &format!("/system/{}", paths.rootfs),
        &format!("/system/roots/{}/rootfs.roothash.p7s", deployment.rootfs.id),
        "mica-root",
        "/newroot",
        &deployment.rootfs.content,
    )?;
    verified_mount(
        control,
        &format!("/system/{}", paths.support),
        &format!(
            "/system/kernels/{}/support.roothash.p7s",
            deployment.kernel.id
        ),
        "mica-support",
        "/support",
        &deployment.kernel.support,
    )?;
    ensure!(
        fs::read_to_string("/support/kernel.release")?.trim() == config.identity.kernel_release,
        "support module release mismatch"
    );
    compose_core(control, &deployment, &paths)?;
    for (source, target) in [
        ("/support/modules", "/newroot/usr/lib/modules"),
        ("/support/firmware", "/newroot/usr/lib/firmware"),
    ] {
        ensure!(
            fs::symlink_metadata(target)?.is_dir() && fs::read_dir(target)?.next().is_none(),
            "invalid support mountpoint {target}"
        );
        run(
            control,
            Startup::Bind {
                source: source.into(),
                target: target.into(),
            },
        )?;
        run(
            control,
            Startup::Remount {
                target: target.into(),
                options: "remount,bind,ro,nodev,nosuid".into(),
            },
        )?;
    }
    (|| -> Result<()> {
        let data = run(
            control,
            Startup::Partition {
                uuid: config.data_part_uuid.clone(),
            },
        )?;
        ensure!(
            data.starts_with("/dev/") && !data.contains(char::is_whitespace),
            "DATA partition not found uniquely"
        );
        let system_node = fs::canonicalize(format!(
            "/sys/class/block/{}",
            Path::new(&device)
                .file_name()
                .context("SYSTEM device name")?
                .to_string_lossy()
        ))?;
        let data_node = fs::canonicalize(format!(
            "/sys/class/block/{}",
            Path::new(&data)
                .file_name()
                .context("DATA device name")?
                .to_string_lossy()
        ))?;
        ensure!(
            system_node.parent() == data_node.parent()
                && fs::read_to_string(system_node.join("partition"))?.trim()
                    == config.board.partitions.system.to_string()
                && fs::read_to_string(data_node.join("partition"))?.trim()
                    == config.board.partitions.data.to_string(),
            "DATA is not on the authenticated system disk"
        );
        // `prjquota` is applied HERE, at mount time, every boot, as the signed
        // policy's `board.dataQuotas` says (on unless the board turns it off);
        // nothing from the product, profile or command line can change it.
        // The projects are made here too, and nowhere else: `data_projects`
        // gives the project directories their ids and sets the limits before
        // the root starts, and /mica/containers shows the option because a
        // bind shares its filesystem's superblock. Without it DATA is mounted
        // without enforcement and carries no limits. Nothing here creates the
        // ext4.
        let quotas = config.board.data_quotas;
        mount(
            control,
            &data,
            "/newroot/mnt/data",
            "ext4",
            if quotas {
                "rw,noatime,prjquota"
            } else {
                "rw,noatime"
            },
        )?;
        if quotas {
            data_projects(Path::new("/newroot/mnt/data"))?;
        }
        persistent_machine_id(Path::new("/newroot/mnt/data/state"), || {
            Ok(fs::read_to_string("/proc/sys/kernel/random/uuid")?
                .trim()
                .replace('-', ""))
        })?;
        ensure!(
            fs::symlink_metadata("/newroot/etc/machine-id")?.is_file(),
            "machine identity mountpoint is not a file"
        );
        run(
            control,
            Startup::Bind {
                source: "/newroot/mnt/data/state/machine-id".into(),
                target: "/newroot/etc/machine-id".into(),
            },
        )?;
        run(
            control,
            Startup::Remount {
                target: "/newroot/etc/machine-id".into(),
                options: "remount,bind,ro,nodev,nosuid,noexec".into(),
            },
        )?;
        Ok(())
    })()
    .context(mica_deploy::deployments::SharedDataFailure)?;
    eprintln!("mica-init: persistent DATA identity ready before system init");
    fs::create_dir_all("/run/mica")?;
    fs::write(
        "/run/mica/boot.json",
        serde_json::to_vec(&serde_json::json!({
            "deploymentId": id, "entry": selected, "kernelId": deployment.kernel.id,
            "rootfsId": deployment.rootfs.id, "contentVerified": true, "secureBoot": secure_boot,
            "backend": backend, "bootVerified": secure_boot || backend == BootKind::UbootFit,
        }))?,
    )?;
    fs::write("/run/mica/deployment.json", &envelope)?;
    // systemd execs /run/initramfs/shutdown at the end of its shutdown and
    // the exit ramdisk releases storage; openrc-init reboots by itself, so an
    // OpenRC root gets no handoff and keeps the memory it would hold.
    let handoff_exitrd = !openrc_root(Path::new("/newroot"))?;
    if handoff_exitrd {
        let manifest = String::from_utf8(bounded_file("/exitrd.files", 8192)?)?;
        let retained_bytes = exitrd_tmpfs_bytes(Path::new("/exitrd"), &manifest)?;
        mount(
            control,
            "tmpfs",
            "/run/initramfs",
            "tmpfs",
            &format!("nosuid,nodev,mode=0700,size={retained_bytes},nr_inodes=8192"),
        )?;
        copy_exitrd(Path::new("/exitrd"), Path::new("/run/initramfs"), &manifest)?;
    } else {
        eprintln!("mica-init: OpenRC root; no exit ramdisk handoff");
    }
    let state = control.observe()?;
    for mount in state.mounts.into_iter().filter(|m| {
        control.storage.backings.contains(&m.device)
            || control
                .storage
                .mappings
                .iter()
                .any(|dm| dm.device == m.device)
    }) {
        if !control.storage.mounts.iter().any(|old| old.id == mount.id) {
            control.storage.mounts.push(mount);
        }
    }
    if handoff_exitrd {
        let mut handoff = control.storage.clone();
        handoff.allow_extra_loops = true;
        fs::write("/run/initramfs/storage.json", serde_json::to_vec(&handoff)?)?;
    }
    startup::validate_new_root(Path::new("/newroot"))?;
    for (source, target) in [
        ("/system", "/newroot/mnt/system"),
        ("/support", "/newroot/run/mica-support"),
    ] {
        // /run moves below; keep the support mount in that tmpfs first.
        let destination = if source == "/support" {
            "/run/mica-support"
        } else {
            target
        };
        if source == "/support" {
            fs::create_dir_all(destination)?;
        }
        ensure!(
            Path::new(destination).is_dir(),
            "missing immutable mountpoint {destination}"
        );
        run(
            control,
            Startup::Move {
                source: source.into(),
                target: destination.into(),
            },
        )?;
    }
    for dir in ["dev", "proc", "sys", "run"] {
        run(
            control,
            Startup::Move {
                source: format!("/{dir}"),
                target: format!("/newroot/{dir}"),
            },
        )?;
    }
    fs::copy("/etc/mica/boot.json", "/newroot/run/mica/boot-policy.json")?;
    eprintln!("mica-init: verified deployment {id}; support mounted before system init");
    if let Ok(status) = fs::read_to_string("/newroot/proc/self/status") {
        let peak = status.lines().find_map(|line| {
            line.strip_prefix("VmHWM:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        });
        if let Some(peak) = peak {
            eprintln!(
                "mica-init: metrics elapsedMs={} peakRssKiB={peak}",
                started.elapsed().as_millis()
            );
        }
    }
    native::switch_root()
}
