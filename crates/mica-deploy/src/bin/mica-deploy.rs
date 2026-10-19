//! Device entry point for signed component installation and boot confirmation.
#![forbid(unsafe_code)]
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use mica_deploy::{
    acquisition::Acquisition,
    board::{BoardFacts, Storage},
    boot::BootKind,
    components::BootIdentity,
    core_set::{self, CoreBoot},
    deployments::{
        BootBackend, BootReceipt, DeploymentStore, SetTarget, SharedDataFailure, Target,
        read_bounded,
    },
    set_state,
    ubi::SysUbi,
    update_set::SetKind,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "mica-deploy",
    version,
    about = "Manage authenticated file deployments"
)]
struct Cli {
    /// The workspace budget in bytes; `probe`, `check`, `fetch` and `import`
    /// require it, and the caller decides it.
    #[arg(long)]
    max_bytes: Option<u64>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Status,
    Probe,
    Discard,
    Booted,
    FailBoot,
    FirmwareReadback {
        #[arg(default_value = "/mnt/data/meta/firmware.json")]
        manifest: PathBuf,
        #[arg(long)]
        record: bool,
    },
    Gc,
    Confirm,
    Reject {
        id: String,
    },
    Rollback,
    Check {
        #[arg(long)]
        source: String,
    },
    Fetch {
        #[arg(long)]
        source: String,
    },
    Import {
        archive: PathBuf,
    },
    /// Read the catalog's line of a kind for a channel.
    SetCheck {
        #[arg(long)]
        kind: SetKind,
        #[arg(long)]
        source: String,
        #[arg(long)]
        channel: String,
        /// Select on this deployment's root rather than the running one.
        #[arg(long)]
        deployment: Option<PathBuf>,
    },
    /// Fetch the set the catalog offers a channel.
    SetFetch {
        #[arg(long)]
        kind: SetKind,
        #[arg(long)]
        source: String,
        #[arg(long)]
        channel: String,
        /// Select on this deployment's root rather than the running one.
        #[arg(long)]
        deployment: Option<PathBuf>,
    },
    Install {
        descriptor: PathBuf,
        #[arg(long)]
        objects: PathBuf,
        /// The core channel's latest set, fetched for this deployment.
        #[arg(long)]
        core_set: Option<PathBuf>,
    },
    /// Publish a signed set beside the current one of its kind and start its
    /// trial. The kind is the one the set was signed as.
    SetInstall {
        set: PathBuf,
        #[arg(long)]
        objects: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Policy {
    identity: BootIdentity,
    board: BoardFacts,
    public_keys: Vec<String>,
    /// SYSTEM's and DATA's GPT partition UUIDs; a `ubi` board has neither.
    #[serde(default)]
    system_part_uuid: String,
    #[serde(default)]
    data_part_uuid: String,
}

fn receipt() -> Result<BootReceipt> {
    Ok(serde_json::from_slice(&read_bounded(
        Path::new("/run/mica/boot.json"),
        4096,
    )?)?)
}
fn command(program: &str, args: &[&str]) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            ensure!(status.success(), "{program} failed: {status}");
            ensure!(output.stdout.len() <= 4096, "excessive command output");
            return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("command timed out: {program}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn remount(path: &str, mode: &str) -> Result<()> {
    command("/bin/mount", &["-o", &format!("remount,{mode}"), path])?;
    Ok(())
}
fn mount_device(text: &str, path: &str, filesystem: &str, writable: bool) -> Result<String> {
    Ok(mount_record(text, path, filesystem, writable)?.0)
}
/// The device number and source of the one mount at `path`, which must be of
/// `filesystem`'s root and, if `writable`, read-write.
fn mount_record(
    text: &str,
    path: &str,
    filesystem: &str,
    writable: bool,
) -> Result<(String, String)> {
    let found = text
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 10 && fields[4] == path).then_some(fields)
        })
        .collect::<Vec<_>>();
    ensure!(found.len() == 1, "shared storage unavailable: {path}");
    let fields = &found[0];
    ensure!(
        fields[3] == "/",
        "shared storage is a subdirectory bind: {path}"
    );
    let separator = fields
        .iter()
        .position(|field| *field == "-")
        .context("invalid mount record")?;
    ensure!(
        fields.get(separator + 1) == Some(&filesystem),
        "wrong filesystem: {path}"
    );
    if writable {
        ensure!(
            fields[5].split(',').any(|option| option == "rw"),
            "shared storage is read-only: {path}"
        );
    }
    let source = fields.get(separator + 2).context("invalid mount record")?;
    Ok((fields[2].to_string(), (*source).to_string()))
}
/// A UBIFS volume has no block device: its mount is bound to the volume by
/// its source, `ubiN:<volume>`.
fn ubifs_mount(text: &str, path: &str, source: &str, writable: bool) -> Result<()> {
    ensure!(
        mount_record(text, path, "ubifs", writable)?.1 == source,
        "mounted volume differs from authenticated boot policy: {path}"
    );
    Ok(())
}
fn require_mount(path: &str, filesystem: &str, writable: bool, number: u32) -> Result<PathBuf> {
    let device = mount_device(
        &std::fs::read_to_string("/proc/self/mountinfo")?,
        path,
        filesystem,
        writable,
    )?;
    let (major, minor) = device.split_once(':').context("invalid mount device")?;
    ensure!(
        !major.is_empty()
            && !minor.is_empty()
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|c| c.is_ascii_digit()),
        "invalid mount device"
    );
    let physical = std::fs::canonicalize(format!("/sys/dev/block/{device}"))?;
    ensure!(
        std::fs::read_to_string(physical.join("partition"))?.trim() == number.to_string(),
        "wrong partition: {path}"
    );
    Ok(physical)
}

/// The product the running, dm-verity authenticated root was built as.
fn product() -> Result<String> {
    let text = read_bounded(Path::new(mica_deploy::components::PRODUCT_FILE), 4096)
        .with_context(|| format!("read {}", mica_deploy::components::PRODUCT_FILE))?;
    Ok(mica_deploy::components::device_product(
        &String::from_utf8(text)?,
    )?)
}

/// The features of the running root's product file.
fn features() -> Result<Vec<String>> {
    let text = read_bounded(Path::new(mica_deploy::components::PRODUCT_FILE), 4096)
        .with_context(|| format!("read {}", mica_deploy::components::PRODUCT_FILE))?;
    Ok(core_set::device_features(&String::from_utf8(text)?)?)
}

/// What the runkit composed this boot.
fn core_boot() -> Result<CoreBoot> {
    Ok(serde_json::from_slice(&read_bounded(
        Path::new(core_set::BOOT_FILE),
        4096,
    )?)?)
}

/// The running deployment, which the runkit authenticated and left in `/run`:
/// its root is what a set is selected on.
fn running_deployment(keys: &[[u8; 32]]) -> Result<mica_deploy::components::Deployment> {
    let envelope = read_bounded(Path::new("/run/mica/deployment.json"), 24576)?;
    Ok(mica_deploy::components::authenticate_deployment(
        &envelope, keys,
    )?)
}

/// The deployment whose root a set is selected on: the one `descriptor`
/// names, or the running one.
fn set_root(
    keys: &[[u8; 32]],
    descriptor: Option<&Path>,
) -> Result<mica_deploy::components::Deployment> {
    match descriptor {
        Some(path) => Ok(mica_deploy::components::authenticate_deployment(
            &read_bounded(path, 24576)?,
            keys,
        )?),
        None => running_deployment(keys),
    }
}

fn policy() -> Result<(Policy, Vec<[u8; 32]>)> {
    let policy: Policy = serde_json::from_slice(&read_bounded(
        Path::new("/run/mica/boot-policy.json"),
        4096,
    )?)?;
    match policy.board.storage {
        Storage::Gpt => ensure!(
            !policy.system_part_uuid.is_empty(),
            "missing SYSTEM binding"
        ),
        Storage::Ubi => ensure!(
            policy.system_part_uuid.is_empty() && policy.data_part_uuid.is_empty(),
            "partition binding on UBI storage"
        ),
    }
    policy.board.validate(&policy.identity.arch)?;
    let keys = policy
        .public_keys
        .iter()
        .map(|key| {
            let bytes = STANDARD.decode(key)?;
            ensure!(STANDARD.encode(&bytes) == *key, "noncanonical metadata key");
            bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid metadata key"))
        })
        .collect::<Result<_>>()?;
    Ok((policy, keys))
}

fn main() -> Result<()> {
    let result = execute();
    if let Err(error) = &result
        && error.is::<SharedDataFailure>()
    {
        std::fs::write("/run/mica/shared-data-failure", format!("{error:#}"))?;
    }
    result
}

fn execute() -> Result<()> {
    let cli = Cli::parse();
    let receipt = receipt().context("authenticated boot receipt is unavailable")?;
    let (policy, keys) = policy()?;
    let backend = policy.board.boot;
    ensure!(
        receipt.backend == backend,
        "boot receipt backend differs from signed policy"
    );
    let boot = match &policy.board.ubi {
        Some(ubi) => {
            let ubi = SysUbi::attached(&ubi.mtd)?;
            let table = std::fs::read_to_string("/proc/self/mountinfo")?;
            let number = ubi.number();
            ubifs_mount(&table, "/mnt/system", &format!("ubi{number}:system"), false)?;
            ubifs_mount(&table, "/mnt/data", &format!("ubi{number}:data"), true)
                .context(SharedDataFailure)?;
            BootBackend::Ubi {
                ubi: std::sync::Arc::new(ubi),
            }
        }
        None => gpt_storage(&policy)?,
    };
    let store = DeploymentStore::new("/mnt/system".into(), boot, "/mnt/data/meta".into())
        .with_reserves(policy.board.reserves)
        .with_boot_trust(policy.board.boot_trust);
    if matches!(cli.command, Action::Booted) {
        ensure!(
            receipt.content_verified,
            "running content is not authenticated"
        );
        mica_deploy::deployments::valid_id(&receipt.deployment_id)?;
        println!("{}", receipt.deployment_id);
        return Ok(());
    }
    if matches!(cli.command, Action::Status) {
        let features = features()?;
        let running = running_deployment(&keys)?;
        let sets = store.sets_status(
            &keys,
            &SetTarget {
                arch: &policy.identity.arch,
                features: &features,
                root: running.rootfs.facts(),
            },
            core_boot()?,
        )?;
        println!(
            "{}",
            json!({"boot":receipt,"state":store.effective_state()?,"deployments":store.describe(&keys)?,"sets":sets})
        );
        return Ok(());
    }
    ensure!(
        receipt.content_verified,
        "running content is not authenticated"
    );
    let _lock = store.lock()?;
    if let Action::FirmwareReadback { manifest, record } = &cli.command {
        let envelope = read_bounded(manifest, 6500)?;
        let firmware = mica_deploy::firmware::authenticate_firmware(&envelope, &keys)?;
        mica_deploy::firmware::admit_firmware(&firmware, &policy.identity, &policy.board)?;
        mica_deploy::firmware::verify_installed(&firmware, &store.boot)?;
        if *record {
            mica_deploy::deployments::atomic_write(
                Path::new("/mnt/data/meta/firmware.json"),
                &envelope,
            )?;
        }
        println!(
            "{}",
            json!({"firmwareId": firmware.id, "generation": firmware.generation, "readbackVerified": true, "receiptRecorded": record})
        );
        return Ok(());
    }
    if matches!(
        cli.command,
        Action::Probe
            | Action::Discard
            | Action::Check { .. }
            | Action::Fetch { .. }
            | Action::Import { .. }
            | Action::SetCheck { .. }
            | Action::SetFetch { .. }
    ) {
        require_workspace(policy.board.storage)?;
        let product = product()?;
        ensure!(
            cli.max_bytes.is_some() || matches!(cli.command, Action::Discard),
            "--max-bytes is required"
        );
        let acquisition = Acquisition {
            root: "/mica/updates".into(),
            store: &store,
            keys: &keys,
            board: &policy.identity.board,
            arch: &policy.identity.arch,
            product: &product,
            max_bytes: cli.max_bytes,
        };
        let result = match cli.command {
            Action::Probe => acquisition.probe()?,
            Action::Discard => json!({"removedFiles":acquisition.discard()?}),
            Action::Check { source } => {
                let catalog = acquisition.check(&source)?;
                json!({"revision":catalog.checkpoint.revision,"selected":catalog.selected})
            }
            Action::Fetch { source } => {
                let catalog = acquisition.check(&source)?;
                match catalog.selected {
                    Some(selected) => json!(acquisition.fetch(selected)?),
                    None => serde_json::Value::Null,
                }
            }
            Action::Import { archive } => {
                ensure!(
                    archive.symlink_metadata()?.is_file(),
                    "archive is not a regular file"
                );
                json!(acquisition.import_any(&mut std::fs::File::open(archive)?)?)
            }
            Action::SetCheck {
                kind,
                source,
                channel,
                deployment,
            } => {
                let (features, root) = (features()?, set_root(&keys, deployment.as_deref())?);
                let catalog = acquisition.set_check(
                    kind,
                    &source,
                    &channel,
                    &SetTarget {
                        arch: &policy.identity.arch,
                        features: &features,
                        root: root.rootfs.facts(),
                    },
                )?;
                json!({"revision":catalog.checkpoint.revision,"selected":catalog.selected,
                    "waitsForSystem":catalog.waits_for_system})
            }
            Action::SetFetch {
                kind,
                source,
                channel,
                deployment,
            } => {
                let (features, root) = (features()?, set_root(&keys, deployment.as_deref())?);
                let catalog = acquisition.set_check(
                    kind,
                    &source,
                    &channel,
                    &SetTarget {
                        arch: &policy.identity.arch,
                        features: &features,
                        root: root.rootfs.facts(),
                    },
                )?;
                match catalog.selected {
                    Some(selected) => json!(acquisition.set_fetch(selected)?),
                    None => serde_json::Value::Null,
                }
            }
            _ => unreachable!(),
        };
        println!("{result}");
        return Ok(());
    }
    let booted_core = core_boot()?;
    // Confirming a boot settles the set on trial, which writes SYSTEM.
    let install = matches!(
        cli.command,
        Action::Install { .. } | Action::Gc | Action::SetInstall { .. }
    ) || (matches!(cli.command, Action::Confirm)
        && store.sets_need_settling(&receipt.deployment_id, &booted_core)?);
    if install {
        remount("/mnt/system", "rw")?;
    }
    if backend == BootKind::Uefi
        && let Err(error) = remount("/boot", "rw")
    {
        if install {
            remount("/mnt/system", "ro")?;
        }
        return Err(error);
    }
    let result: Result<_> = (|| match cli.command {
        Action::Confirm => {
            let state = store.confirm(&receipt)?;
            // After the deployment: the health gate's verdict is on the boot
            // as a whole, and a deployment that could not be confirmed
            // confirms nothing composed over it.
            // The answer is the deployment state; `status` reports the sets.
            store.settle_sets(&receipt.deployment_id, &booted_core, &keys)?;
            Ok(json!(state))
        }
        // A boot that failed with a set alone on trial is the set's failure,
        // and its trial has already paid for it: the deployment is kept.
        Action::FailBoot if booted_core.pending && !booted_core.paired => {
            Ok(json!({"retired": false, "setTrial": "core"}))
        }
        Action::FailBoot => store
            .fail_boot(&receipt)
            .map(|retired| json!({"retired": retired})),
        Action::Reject { id } => store.reject(&id).map(|value| json!(value)),
        Action::Rollback => store.rollback(&receipt).map(|value| json!(value)),
        Action::Install {
            descriptor,
            objects,
            core_set,
        } => store
            .install(
                &read_bounded(&descriptor, 24576)?,
                &keys,
                &Target {
                    board: &policy.identity.board,
                    arch: &policy.identity.arch,
                    product: &product()?,
                    features: &features()?,
                },
                &objects,
                &receipt,
                core_set
                    .map(|path| read_bounded(&path, set_state::MAX_ENVELOPE_BYTES))
                    .transpose()?
                    .as_deref(),
            )
            .map(|value| json!(value)),
        Action::Gc => store
            .collect(&receipt, &keys)
            .map(|count| json!({"removedFiles":count})),
        Action::SetInstall { set, objects } => store
            .install_set(
                &read_bounded(&set, set_state::MAX_ENVELOPE_BYTES)?,
                &keys,
                &SetTarget {
                    arch: &policy.identity.arch,
                    features: &features()?,
                    root: running_deployment(&keys)?.rootfs.facts(),
                },
                &objects,
            )
            .map(|value| json!(value)),
        Action::Status
        | Action::Probe
        | Action::Discard
        | Action::Booted
        | Action::FirmwareReadback { .. }
        | Action::Check { .. }
        | Action::Fetch { .. }
        | Action::Import { .. }
        | Action::SetCheck { .. }
        | Action::SetFetch { .. } => unreachable!(),
    })();
    let esp_sync = if backend == BootKind::Uefi {
        remount("/boot", "ro")
    } else {
        Ok(())
    };
    let system_sync = if install {
        remount("/mnt/system", "ro")
    } else {
        Ok(())
    };
    esp_sync?;
    system_sync?;
    println!("{}", serde_json::to_string(&result?)?);
    Ok(())
}

/// The boot medium of a `gpt` board, with SYSTEM and DATA mounted from the
/// partitions its policy binds.
fn gpt_storage(policy: &Policy) -> Result<BootBackend> {
    let partitions = policy.board.gpt_partitions()?;
    let system = require_mount("/mnt/system", "ext4", false, partitions.system)?;
    let boot = match policy.board.boot {
        BootKind::Uefi => {
            let esp = require_mount("/boot", "vfat", false, partitions.boot)?;
            ensure!(
                system.parent() == esp.parent(),
                "ESP and SYSTEM are on different disks"
            );
            BootBackend::Uefi {
                esp: "/boot".into(),
            }
        }
        BootKind::UbootFit => BootBackend::Fit {
            layout: policy
                .board
                .records
                .context("FIT boot records absent from the boot policy")?,
            firmware: mica_deploy::deployments::boot_partition(&system, &policy.board)?,
        },
    };
    let data_check = (|| -> Result<()> {
        let data = require_mount("/mnt/data", "ext4", true, partitions.data)?;
        ensure!(
            system.parent() == data.parent(),
            "DATA and SYSTEM are on different disks"
        );
        let node = data
            .file_name()
            .context("missing DATA device")?
            .to_str()
            .context("invalid DATA device")?;
        ensure!(
            command(
                "/sbin/blkid",
                &["-s", "PARTUUID", "-o", "value", &format!("/dev/{node}")]
            )? == policy.data_part_uuid.to_ascii_lowercase(),
            "DATA differs from authenticated boot policy"
        );
        Ok(())
    })();
    data_check.context(SharedDataFailure)?;
    let node = system
        .file_name()
        .context("missing SYSTEM device")?
        .to_str()
        .context("invalid SYSTEM device")?;
    ensure!(
        command(
            "/sbin/blkid",
            &["-s", "PARTUUID", "-o", "value", &format!("/dev/{node}")]
        )? == policy.system_part_uuid.to_ascii_lowercase(),
        "mounted SYSTEM differs from authenticated boot policy"
    );
    Ok(boot)
}

fn require_workspace(storage: Storage) -> Result<()> {
    let table = std::fs::read_to_string("/proc/self/mountinfo")?;
    let filesystem = match storage {
        Storage::Gpt => "ext4",
        Storage::Ubi => "ubifs",
    };
    let data = mount_device(&table, "/mnt/data", filesystem, true)?;
    let mut namespace = None;
    for line in table.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        if fields[4] == "/mica" {
            ensure!(
                namespace.is_none()
                    && fields[2] == data
                    && fields[3] == "/mica"
                    && fields[5].split(',').any(|option| option == "rw"),
                "invalid DATA namespace binding"
            );
            namespace = Some(());
        }
        ensure!(
            fields[4] != "/mica/updates" && !fields[4].starts_with("/mica/updates/"),
            "unexpected mount in update workspace"
        );
    }
    ensure!(namespace.is_some(), "DATA namespace is not mounted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mount_device, ubifs_mount};

    #[test]
    fn storage_requires_one_complete_physical_mount() {
        let mount = "35 22 253:2 / /mnt/system ro,noatime - ext4 /dev/vda2 ro\n";
        assert_eq!(
            mount_device(mount, "/mnt/system", "ext4", false).unwrap(),
            "253:2"
        );
        assert!(mount_device(mount, "/mnt/system", "ext4", true).is_err());
        assert!(mount_device(mount, "/mnt/system", "vfat", false).is_err());
        assert!(mount_device(&mount.repeat(2), "/mnt/system", "ext4", false).is_err());
        assert!(
            mount_device(
                &mount.replace(" / /mnt", " /roots /mnt"),
                "/mnt/system",
                "ext4",
                false
            )
            .is_err()
        );
    }

    #[test]
    fn a_ubifs_mount_is_bound_by_its_volume() {
        let mount = "36 22 0:21 / /mnt/data rw,noatime - ubifs ubi0:data rw\n";
        ubifs_mount(mount, "/mnt/data", "ubi0:data", true).unwrap();
        assert!(ubifs_mount(mount, "/mnt/data", "ubi1:data", true).is_err());
        assert!(ubifs_mount(mount, "/mnt/data", "ubi0:system", false).is_err());
        let ext4 = mount.replace("ubifs", "ext4");
        assert!(ubifs_mount(&ext4, "/mnt/data", "ubi0:data", true).is_err());
        let read_only = mount.replace(" rw,noatime ", " ro,noatime ");
        assert!(ubifs_mount(&read_only, "/mnt/data", "ubi0:data", true).is_err());
        ubifs_mount(&read_only, "/mnt/data", "ubi0:data", false).unwrap();
    }
}
