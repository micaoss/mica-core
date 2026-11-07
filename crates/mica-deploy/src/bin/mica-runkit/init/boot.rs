//! The boot: selecting, verifying and switching to a deployment.

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    board::{BootTrust, Storage},
    boot::{
        BootKind, copy_exitrd, data_projects, exitrd_tmpfs_bytes, fit_selected, openrc_root,
        persistent_machine_id, selected_entry, shutdown,
        startup::{
            self,
            native::{self, Operation as Startup},
        },
        trust::{self, BuiltinTrust, Enrolment, KernelKeyring},
        utf16_variable,
    },
    components::{component_id, verify_deployment},
    core_set::{self, BootRoot, CoreBoot, device_features},
    factory_reset,
    ubi::{SysUbi, UbiDevice},
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
            match config.board.storage {
                Storage::Gpt =>
                    uuid.len() == 36 && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
                Storage::Ubi => uuid.is_empty(),
            },
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
    // A UBI board's kernel attached its UBI device from its built-in command
    // line before init; nothing here attaches one.
    let ubi = match (&config.board.storage, &config.board.ubi) {
        (Storage::Ubi, Some(ubi)) => {
            let discovery = Instant::now();
            loop {
                match SysUbi::attached(&ubi.mtd) {
                    Ok(device) => break Some(device),
                    Err(error) if discovery.elapsed() >= Duration::from_secs(15) => {
                        return Err(error.context(SharedSystemFailure));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(100)),
                }
            }
        }
        _ => None,
    };
    let mut device = String::new();
    let discovery = Instant::now();
    while ubi.is_none() && discovery.elapsed() < Duration::from_secs(15) {
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
    if let Some(ubi) = &ubi {
        // UBIFS replays its journal before completing this read-only mount.
        device = format!("ubi{}:system", ubi.number());
        mount(
            control,
            &device,
            "/system",
            "ubifs",
            "ro,nodev,nosuid,noexec",
        )
        .context(SharedSystemFailure)?;
    } else {
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
    }
    *attempt = Some(BootAttempt {
        id: id.clone(),
        system_device: device.clone(),
        board: config.board.clone(),
        trial_alone: false,
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
    // Until the owner keyring is sealed, every mapping is the system's.
    let mut trust = BuiltinTrust::take().context("the system's trust is spent")?;
    let (rootfs, support) = match &ubi {
        Some(_) => (
            format!("r-{}", deployment.rootfs.id),
            format!("s-{}", deployment.kernel.id),
        ),
        None => (
            format!("/system/{}", paths.rootfs),
            format!("/system/{}", paths.support),
        ),
    };
    verified_mount(
        control,
        &mut trust,
        ubi.as_ref(),
        &rootfs,
        &format!("/system/roots/{}/rootfs.roothash.p7s", deployment.rootfs.id),
        "mica-root",
        "/newroot",
        &deployment.rootfs.content,
    )?;
    verified_mount(
        control,
        &mut trust,
        ubi.as_ref(),
        &support,
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
    // A factory reset staged on SYSTEM makes DATA again before anything
    // mounts it (`docs/mica-core.md` 3.7). One staged while another
    // deployment ran waits for a boot of that one; one that cannot be read
    // resets nothing.
    let reset = match factory_reset::staged(Path::new("/system")) {
        Ok(Some(request)) if request.deployment.as_deref() == Some(id.as_str()) => Some(request),
        Ok(Some(_)) => {
            eprintln!("mica-init: factory reset staged for another deployment; not at this boot");
            None
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!("mica-init: factory reset request unreadable, not resetting: {error:#}");
            None
        }
    };
    // Whether DATA was made again; `false`, its format failed and it is
    // erased in place once mounted.
    let mut formatted = None;
    if let (Some(_), Some(policy), Some(_)) = (&ubi, &config.board.ubi, &reset) {
        formatted = Some(
            match run(
                control,
                Startup::UbiWipeData {
                    mtd: policy.mtd.clone(),
                },
            ) {
                Ok(done) => {
                    eprintln!("mica-init: factory reset: DATA {done}");
                    true
                }
                Err(error) => {
                    eprintln!("mica-init: factory reset: DATA not emptied: {error:#}");
                    false
                }
            },
        );
    }
    // DATA is mounted before the core components are chosen: the trial of a
    // pending core set is counted there, and the count is spent before the
    // set is tried. Nothing is bound into `/usr` or `/etc` yet, which the
    // composition needs.
    // A UBI board grows its empty DATA before the kernel formats it at the
    // mount below. DATA at its factory size is a working system, so a failure
    // is logged and the boot goes on.
    if let (Some(_), Some(policy)) = (&ubi, &config.board.ubi)
        && let Some(reserve) = policy.store_reserve
    {
        match run(
            control,
            Startup::UbiGrowData {
                mtd: policy.mtd.clone(),
                reserve,
            },
        ) {
            Ok(done) => eprintln!("mica-init: DATA {}", done.trim()),
            Err(error) => eprintln!("mica-init: DATA not grown: {error:#}"),
        }
    }
    let quotas = (|| -> Result<bool> {
        if let Some(ubi) = &ubi {
            // UBIFS has no project quotas: the policy turns them off.
            mount(
                control,
                &format!("ubi{}:data", ubi.number()),
                "/newroot/mnt/data",
                "ubifs",
                "rw,noatime",
            )?;
            return Ok(false);
        }
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
                    == config.board.gpt_partitions()?.system.to_string()
                && fs::read_to_string(data_node.join("partition"))?.trim()
                    == config.board.gpt_partitions()?.data.to_string(),
            "DATA is not on the authenticated system disk"
        );
        if reset.is_some() {
            formatted = Some(
                match run(
                    control,
                    Startup::FormatData {
                        device: data.clone(),
                        arch: config.identity.arch.clone(),
                        format: config.board.data_format(),
                    },
                ) {
                    Ok(done) => {
                        eprintln!("mica-init: factory reset: {done}");
                        true
                    }
                    Err(error) => {
                        eprintln!("mica-init: factory reset: DATA not formatted: {error:#}");
                        false
                    }
                },
            );
        }
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
        Ok(quotas)
    })()
    .context(mica_deploy::deployments::SharedDataFailure)?;
    // A staged factory reset is carried out here, before the projects, the
    // core composition or the machine identity read DATA, and before any
    // process of the root exists. An erase the runkit cannot finish leaves
    // the request in `meta/`, and the next boot resumes it.
    (|| -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let supervisor = &mut control.supervisor;
        if let Some((request, formatted)) = reset.as_ref().zip(formatted) {
            let data = Path::new("/newroot/mnt/data");
            let unremoved = if formatted {
                0
            } else {
                factory_reset::erase_in_place(
                    data,
                    &mut || supervisor.kick(),
                    &mut |line| eprintln!("mica-init: {line}"),
                )?
            };
            factory_reset::restore(Path::new("/system"), data)?;
            fs::create_dir_all("/run/mica")?;
            fs::write(
                factory_reset::PROGRESS,
                serde_json::to_vec(&factory_reset::Progress {
                    v: 2,
                    requested: request.requested,
                    formatted,
                    unremoved,
                })?,
            )?;
            eprintln!(
                "mica-init: factory reset to deployment {id}: DATA {}, {unremoved} entries not removed",
                if formatted { "made again" } else { "erased in place" }
            );
        } else if let Some(done) = factory_reset::apply(
            Path::new("/newroot/mnt/data"),
            now,
            &mut || supervisor.kick(),
            &mut |line| eprintln!("mica-init: {line}"),
        )? {
            eprintln!(
                "mica-init: factory reset done, requested by {:?}, {} entries not removed",
                done.by, done.unremoved
            );
        }
        if quotas {
            data_projects(Path::new("/newroot/mnt/data"))?;
        }
        Ok(())
    })()
    .context(mica_deploy::deployments::SharedDataFailure)?;
    // A root whose product file cannot be read selects nothing from a core
    // set; refusing the boot for it would retire a deployment that is sound.
    let features = bounded_file("/newroot/usr/lib/mica/product.conf", 4096)
        .and_then(|bytes| Ok(device_features(&String::from_utf8(bytes)?)?))
        .unwrap_or_else(|error| {
            eprintln!("mica-init: product features unreadable: {error:#}");
            Vec::new()
        });
    let composition = core_set::composition(
        Path::new("/system"),
        Path::new("/newroot/mnt/data/meta"),
        ubi.as_ref().map(|device| device as &dyn UbiDevice),
        &keys,
        &BootRoot {
            arch: &config.identity.arch,
            features: &features,
            root: deployment.rootfs.facts(),
        },
        &id,
        |refusal| eprintln!("mica-init: {refusal}"),
    )?;
    // From here a failed boot of a set alone on trial is the set's: it has
    // spent a boot of its trial, and the deployment under it is not retired
    // for it. A paired set's trial is the deployment's.
    if let Some(attempt) = attempt.as_mut() {
        attempt.trial_alone = composition.pending && !composition.paired;
    }
    eprintln!(
        "mica-init: core set {} ({} generation {}){}",
        composition.id,
        composition.set.channel,
        composition.set.generation,
        if composition.paired {
            ", on trial with this deployment"
        } else if composition.pending {
            ", on trial"
        } else {
            ""
        }
    );
    compose_core(
        control,
        &mut trust,
        ubi.as_ref(),
        &core_set::select(&composition.set, &features, &deployment.rootfs.facts())?,
    )?;
    let core_boot = CoreBoot {
        core_set_id: composition.id.clone(),
        pending: composition.pending,
        paired: composition.paired,
    };
    // The support image, the root and every core component are mapped: the
    // owner's certificates go in now, and the keyring is sealed before any
    // driver extension maps. A factory reset boot offers none and maps none.
    let mut unconfirmed = Vec::new();
    let enrolment = if reset.is_some() {
        Enrolment::None
    } else {
        let listed = trust::owner_certs(Path::new("/system")).unwrap_or_else(|error| {
            eprintln!("mica-init: enrolled certificates unreadable: {error:#}");
            Vec::new()
        });
        if config.product.owner_enrolment.require_physical_confirmation {
            let meta = Path::new("/newroot/mnt/data/meta");
            let mut confirmed = trust::confirmed(meta);
            let held = confirmed.clone();
            let (offered, waiting) =
                trust::confirm_enrolment(listed, &mut confirmed, |sha256, der| {
                    confirm_on_console(control, sha256, der)
                });
            if confirmed != held
                && let Err(error) = trust::write_confirmed(meta, &confirmed)
            {
                eprintln!("mica-init: confirmed certificates not recorded: {error:#}");
            }
            unconfirmed = waiting;
            Enrolment::Load(offered)
        } else {
            Enrolment::Load(listed)
        }
    };
    let mut sealed = trust::enrol_and_seal(trust, &mut KernelKeyring, enrolment);
    sealed
        .trust
        .owner_certs
        .extend(unconfirmed.into_iter().map(|sha256| trust::OwnerCert {
            sha256,
            state: trust::CertState::Unconfirmed,
            reason: Some("not confirmed on the device's console".into()),
        }));
    eprintln!(
        "mica-init: owner keyring {:?}, {} of {} enrolled certificate(s) loaded",
        sealed.trust.owner_keyring,
        sealed
            .trust
            .owner_certs
            .iter()
            .filter(|cert| cert.state == trust::CertState::Loaded)
            .count(),
        sealed.trust.owner_certs.len()
    );
    let extensions = extensions::compose(
        control,
        &sealed,
        ubi.as_ref(),
        &id,
        &deployment.kernel,
        reset.is_some(),
    )?;
    // A failed boot of a change alone on trial is the change's, as a core
    // set's is.
    if let Some(attempt) = attempt.as_mut()
        && extensions.trial.as_ref().is_some_and(|trial| !trial.paired)
    {
        attempt.trial_alone = true;
    }
    (|| -> Result<()> {
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
            "backend": backend, "bootVerified": secure_boot
                || (backend == BootKind::UbootFit && config.board.boot_trust == BootTrust::Fit),
        }))?,
    )?;
    // What this runkit does for the running system, apart from the boot
    // receipt, whose readers refuse fields they do not know.
    fs::write(
        factory_reset::CAPABILITIES,
        serde_json::to_vec(&serde_json::json!({ "factoryReset": true, "factoryResetVersion": 2 }))?,
    )?;
    fs::write("/run/mica/deployment.json", &envelope)?;
    fs::write(core_set::BOOT_FILE, serde_json::to_vec(&core_boot)?)?;
    fs::write(trust::TRUST, serde_json::to_vec(&sealed.trust)?)?;
    fs::write(
        mica_deploy::extensions::BOOT_FILE,
        serde_json::to_vec(&extensions)?,
    )?;
    fs::write("/run/mica/core-set.json", &composition.envelope)?;
    // systemd execs /run/initramfs/shutdown at the end of its shutdown and
    // the exit ramdisk releases storage; openrc-init reboots by itself, so an
    // OpenRC root gets no handoff and keeps the memory it would hold.
    // A UBI board's root is released by its own init, which remounts UBIFS
    // read-only: there is no loop or disk for an exit ramdisk to release.
    let handoff_exitrd = ubi.is_none() && !openrc_root(Path::new("/newroot"))?;
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
    } else if ubi.is_some() {
        eprintln!("mica-init: UBI storage; no exit ramdisk handoff");
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

/// How long the console is given to confirm an enrolled certificate.
const CONFIRM_SECONDS: u64 = 30;

/// Ask on the console whether the enrolled certificate `sha256` (`der`) is
/// to be trusted: Enter within [`CONFIRM_SECONDS`] confirms it, with the
/// watchdog fed while it waits. Anything typed before the question is not an
/// answer.
fn confirm_on_console(control: &mut BootControl, sha256: &str, der: &[u8]) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let subject = mica_deploy::owner_certs::describe(der, None)
        .map_or_else(|_| "an unreadable subject".to_owned(), |d| d.subject);
    let stdin = rustix::stdio::stdin();
    let ready = |seconds: i64| {
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: seconds,
                tv_nsec: 0,
            }),
        )
        .is_ok_and(|n| n > 0)
    };
    let mut buffer = [0_u8; 256];
    while ready(0) {
        if rustix::io::read(stdin, &mut buffer).map_or(true, |n| n == 0) {
            break;
        }
    }
    eprintln!(
        "mica-init: owner certificate {sha256} ({subject}) is enrolled; press Enter on this console within {CONFIRM_SECONDS} s to trust it"
    );
    for _ in 0..CONFIRM_SECONDS {
        let _ = control.supervisor.kick();
        if ready(1) {
            let confirmed = rustix::io::read(stdin, &mut buffer).is_ok_and(|n| n > 0);
            eprintln!(
                "mica-init: owner certificate {sha256} {}",
                if confirmed {
                    "confirmed"
                } else {
                    "not confirmed"
                }
            );
            return confirmed;
        }
    }
    eprintln!(
        "mica-init: owner certificate {sha256} not confirmed; it is asked again at the next boot"
    );
    false
}
