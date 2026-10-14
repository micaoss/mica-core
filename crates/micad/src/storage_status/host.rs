//! The storage status of the running host.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

use super::*;

/// Read-only source of storage evidence.
#[async_trait::async_trait]
pub trait StorageStatusSource: Send + Sync {
    /// Observe the current storage evidence.
    ///
    /// # Errors
    ///
    /// Returns an error only when this daemon has no observer at all; a
    /// production observer answers with absent evidence instead.
    async fn observe(&self) -> Result<StorageEvidence>;
}

/// The observer a daemon without host access has: none.
pub struct UnavailableStorageStatus;

#[async_trait::async_trait]
impl StorageStatusSource for UnavailableStorageStatus {
    async fn observe(&self) -> Result<StorageEvidence> {
        Err(anyhow::anyhow!("this daemon observes no storage"))
    }
}

/// Production observer over sysfs, `/proc/self/mountinfo`, `df` and systemd.
pub struct HostStorage {
    pub(super) root: std::path::PathBuf,
    pub(super) space: SpaceReader,
}

/// Answers one mount point's space. Injected so a fixture tree cannot make a
/// test shell out to `df` against the machine running it.
pub(super) type SpaceReader = Box<dyn Fn(&str) -> Option<FsSpace> + Send + Sync>;

impl HostStorage {
    /// The observer `main.rs` attaches on a device.
    #[must_use]
    pub fn production() -> Self {
        Self::at("/").with_space_reader(df_space)
    }

    /// An observer rooted at `root`, with NO space reader: nothing here shells
    /// out until one is attached, so a fixture tree cannot make a test run
    /// `df` against the machine it is running on.
    #[must_use]
    pub fn at(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            space: Box::new(|_| None),
        }
    }

    /// Attach the reader that answers a mount point's space.
    #[must_use]
    pub fn with_space_reader(
        mut self,
        reader: impl Fn(&str) -> Option<FsSpace> + Send + Sync + 'static,
    ) -> Self {
        self.space = Box::new(reader);
        self
    }

    pub(super) fn path(&self, relative: &str) -> std::path::PathBuf {
        self.root.join(relative)
    }

    pub(super) fn read_trimmed(&self, relative: &str) -> Option<String> {
        mica_fs::read_trimmed(&self.path(relative))
    }

    /// Resolve a `/dev/disk/by-*` symlink to its `/dev/<name>` target, under
    /// this observer's root.
    pub(super) fn resolve_dev(&self, device: &str) -> Option<String> {
        let relative = device.strip_prefix('/')?;
        let target = std::fs::read_link(self.path(relative)).ok()?;
        let name = target.file_name()?.to_str()?;
        Some(format!("/dev/{name}"))
    }

    /// The device of `spec`'s tier: from the boot policy when there is one,
    /// by GPT name when there is not.
    pub(super) fn tier_device(
        &self,
        spec: &TierSpec,
        policy: Option<&PolicyPartitions>,
    ) -> Option<String> {
        let Some(policy) = policy else {
            return self.resolve_dev(&format!("/dev/disk/by-partlabel/{}", spec.partition_label));
        };
        let by_uuid = |uuid: &str| self.resolve_dev(&format!("/dev/disk/by-partuuid/{uuid}"));
        match spec.name {
            "system" => by_uuid(&policy.system_uuid),
            "data" => by_uuid(&policy.data_uuid),
            name if name == policy.boot_tier => {
                let system = by_uuid(&policy.system_uuid)?;
                self.sibling_partition(&system, policy.boot_number)
            }
            // The other boot tier: not on this board, which is an answer.
            _ => None,
        }
    }

    /// The partition numbered `number` on the disk holding `device`.
    pub(super) fn sibling_partition(&self, device: &str, number: u32) -> Option<String> {
        let name = device.strip_prefix("/dev/")?;
        let node = std::fs::canonicalize(self.path("sys/class/block").join(name)).ok()?;
        let disk = node.parent()?;
        let wanted = number.to_string();
        std::fs::read_dir(disk).ok()?.flatten().find_map(|entry| {
            let number = std::fs::read_to_string(entry.path().join("partition")).ok()?;
            (number.trim() == wanted)
                .then(|| format!("/dev/{}", entry.file_name().to_string_lossy()))
        })
    }

    /// `device`'s GPT name, from its sysfs `uevent`.
    pub(super) fn partition_name(&self, device: &str) -> Option<String> {
        let name = device.strip_prefix("/dev/")?;
        let uevent =
            std::fs::read_to_string(self.path("sys/class/block").join(name).join("uevent")).ok()?;
        uevent
            .lines()
            .find_map(|line| line.strip_prefix("PARTNAME="))
            .map(str::to_string)
    }

    /// Every whole-disk medium, with its partitions' sizes.
    pub(super) fn media(&self) -> (Vec<MediumEvidence>, BTreeMap<String, u64>) {
        let mut media = Vec::new();
        let mut partitions = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(self.path("sys/block")) else {
            return (media, partitions);
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !is_virtual_block(name))
            .collect();
        names.sort();
        for name in names {
            for (part, size) in self.partitions_of(&name) {
                partitions.insert(part, size);
            }
            media.push(self.medium(&name));
        }
        (media, partitions)
    }

    pub(super) fn partitions_of(&self, disk: &str) -> Vec<(String, u64)> {
        let Ok(entries) = std::fs::read_dir(self.path(&format!("sys/block/{disk}"))) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let relative = format!("sys/block/{disk}/{name}");
                self.path(&format!("{relative}/partition"))
                    .exists()
                    .then(|| {
                        let sectors: u64 = self
                            .read_trimmed(&format!("{relative}/size"))
                            .and_then(|text| text.parse().ok())
                            .unwrap_or_default();
                        (format!("/dev/{name}"), sectors * SECTOR_BYTES)
                    })
            })
            .collect()
    }

    pub(super) fn medium(&self, name: &str) -> MediumEvidence {
        let block = format!("sys/block/{name}");
        MediumEvidence {
            kind: medium_kind(name).to_string(),
            size_bytes: self
                .read_trimmed(&format!("{block}/size"))
                .and_then(|text| text.parse::<u64>().ok())
                .map(|sectors| sectors * SECTOR_BYTES),
            model: self
                .read_trimmed(&format!("{block}/device/model"))
                .or_else(|| self.read_trimmed(&format!("{block}/device/name"))),
            rotational: self
                .read_trimmed(&format!("{block}/queue/rotational"))
                .map(|text| text == "1"),
            health: match self.read_trimmed(&format!("{block}/device/life_time")) {
                Some(life_time_raw) => MediaHealth::Emmc {
                    life_time_raw,
                    pre_eol_raw: self.read_trimmed(&format!("{block}/device/pre_eol_info")),
                },
                None => MediaHealth::Unsupported(unsupported_reason(name).to_string()),
            },
            name: name.to_string(),
        }
    }

    pub(super) fn mounts(&self) -> Vec<MountEvidence> {
        let Ok(text) = std::fs::read_to_string(self.path("proc/self/mountinfo")) else {
            return Vec::new();
        };
        parse_mountinfo(&text)
    }

    /// Observe one bind namespace.
    pub(super) fn bind(&self, spec: &BindSpec, mounts: &[MountEvidence]) -> BindEvidence {
        let mount = mounts
            .iter()
            .find(|mount| mount.mount == spec.mount)
            .cloned();
        // A symlink here is a substituted namespace, which `mica-data-layout`
        // refuses outright; `symlink_metadata` is what makes that visible,
        // because `metadata` would follow the link and call it a directory.
        let source_is_directory = spec
            .source
            .strip_prefix('/')
            .and_then(|relative| std::fs::symlink_metadata(self.path(relative)).ok())
            .map(|meta| meta.is_dir());
        BindEvidence {
            probe: self.probe(spec, mount.as_ref()),
            mount,
            source_is_directory,
        }
    }

    /// The readiness probe, on the system namespace only.
    ///
    /// `/srv` is user-owned: micad writing a probe file into it would put a
    /// daemon's private file in a namespace the product gives to the
    /// operator, so `/srv` gets no probe and says so rather than getting a
    /// silent pass.
    pub(super) fn probe(
        &self,
        spec: &BindSpec,
        mount: Option<&MountEvidence>,
    ) -> Option<ProbeOutcome> {
        if spec.name != "mica" {
            return Some(ProbeOutcome::NotAttempted(format!(
                "{} is outside the system readiness probe namespace; no probe write is authorized",
                spec.mount
            )));
        }
        let Some(mount) = mount else {
            return Some(ProbeOutcome::NotAttempted(format!(
                "{} is not mounted",
                spec.mount
            )));
        };
        if !mount_matches_namespace(mount) {
            return Some(ProbeOutcome::NotAttempted(
                "unexpected namespace source".to_string(),
            ));
        }
        if mount.read_only {
            return Some(ProbeOutcome::NotAttempted(format!(
                "{} is mounted read-only",
                spec.mount
            )));
        }
        let subtree = self.path(PROBE_SUBTREE.trim_start_matches('/'));
        if !subtree.is_dir() {
            return Some(ProbeOutcome::NotAttempted(format!(
                "{PROBE_SUBTREE} does not exist; mica-data-layout has not run"
            )));
        }
        Some(match write_probe(&subtree) {
            Ok(()) => ProbeOutcome::Passed,
            Err(err) => ProbeOutcome::Failed(format!("{err:#}")),
        })
    }

    /// The fsck units systemd recorded, over the system bus. Soft: a bus that
    /// does not answer yields no check evidence, never an error.
    pub(super) async fn checks(&self) -> Vec<CheckEvidence> {
        let Ok(connection) = zbus::Connection::system().await else {
            return Vec::new();
        };
        let Ok(manager) = zbus::Proxy::new(
            &connection,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        else {
            return Vec::new();
        };
        let listed: zbus::Result<Vec<SystemdUnit>> = manager
            .call(
                "ListUnitsByPatterns",
                &(Vec::<String>::new(), vec![FSCK_UNIT_PATTERN.to_string()]),
            )
            .await;
        let Ok(listed) = listed else {
            return Vec::new();
        };
        let mut checks = Vec::new();
        for unit in listed {
            let mut check = CheckEvidence {
                unit: unit.0,
                active_state: Some(unit.3),
                result: None,
                exit_status: None,
            };
            if let Ok(service) = zbus::Proxy::new(
                &connection,
                "org.freedesktop.systemd1",
                unit.6.clone(),
                "org.freedesktop.systemd1.Service",
            )
            .await
            {
                check.result = service.get_property::<String>("Result").await.ok();
                check.exit_status = service.get_property::<i32>("ExecMainStatus").await.ok();
            }
            checks.push(check);
        }
        checks
    }
}

/// `ListUnitsByPatterns` returns `a(ssssssouso)`: name, description, load
/// state, active state, sub state, followed unit, object path, job id, job
/// type, job object path. Only the name, the active state and the object path
/// are read.
pub(super) type SystemdUnit = (
    String,
    String,
    String,
    String,
    String,
    String,
    zbus::zvariant::OwnedObjectPath,
    u32,
    String,
    zbus::zvariant::OwnedObjectPath,
);

/// The unit family systemd runs one instance of per fstab entry with a
/// non-zero pass number.
pub(super) const FSCK_UNIT_PATTERN: &str = "systemd-fsck@*.service";

/// sysfs reports block sizes in 512-byte sectors regardless of the device's
/// own block size.
pub(super) const SECTOR_BYTES: u64 = 512;

/// Block devices that are not physical media: nothing under these names has
/// wear to report or a lifetime to run out.
pub(super) fn is_virtual_block(name: &str) -> bool {
    ["loop", "ram", "zram", "dm-", "md", "sr"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The medium class a kernel block name implies.
pub(super) fn medium_kind(name: &str) -> &'static str {
    if name.starts_with("mmcblk") {
        "mmc"
    } else if name.starts_with("nvme") {
        "nvme"
    } else if name.starts_with("sd") {
        "scsi"
    } else {
        "other"
    }
}

/// Why a medium has no normalized health, said plainly.
///
/// NVMe and SATA wear lives behind SMART, and this image ships no reader for
/// it — no `smartctl`, no `nvme-cli`, and micad links no SMART library. That
/// is a build decision with a name, so the surface reports the decision
/// rather than an empty health object that reads as "fine".
pub(super) fn unsupported_reason(name: &str) -> &'static str {
    match medium_kind(name) {
        "mmc" => "the mmc driver exports no life_time for this device",
        "nvme" | "scsi" => {
            "SMART is not readable: this image ships no smartctl or nvme-cli, by design"
        }
        _ => "no health source is defined for this medium class",
    }
}

/// The probe write, in full: create exclusively, fsync the file,
/// remove it, fsync the directory.
pub(super) fn write_probe(subtree: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = subtree.join(format!(".micad-storage-probe.{}", std::process::id()));
    let outcome = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(b"micad storage readiness probe\n")
            .with_context(|| format!("write {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", path.display()))?;
        Ok(())
    })();
    // The removal runs whatever the write did, so a failed probe does not
    // leave its own evidence behind on the filesystem it just failed on.
    let removed = std::fs::remove_file(&path);
    outcome?;
    removed.with_context(|| format!("remove {}", path.display()))?;
    std::fs::File::open(subtree)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("fsync {}", subtree.display()))?;
    Ok(())
}

/// One mount point's space, from `df -P -B1`.
pub(super) fn df_space(mount: &str) -> Option<FsSpace> {
    let output = std::process::Command::new("df")
        .args(["-P", "-B1", mount])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&output.stdout))
}

pub(super) async fn bounded_storage_output(mut command: tokio::process::Command) -> Option<String> {
    command
        .kill_on_drop(true)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > 16384 {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[async_trait::async_trait]
impl StorageStatusSource for HostStorage {
    async fn observe(&self) -> Result<StorageEvidence> {
        let (media, partition_sizes) = self.media();
        let mounts = self.mounts();
        let checks = checks_by_device(&self.checks().await, |device| self.resolve_dev(device));

        let policy = self
            .read_trimmed(BOOT_POLICY_PATH)
            .and_then(|text| PolicyPartitions::parse(&text));
        let mut tiers = BTreeMap::new();
        for spec in TIERS {
            let Some(device) = self.tier_device(spec, policy.as_ref()) else {
                // No partition with this label: the tier is not on this board,
                // which is an answer and is reported as one.
                continue;
            };
            // The tier's OWN mountpoint, preferred over any other mount of the
            // same device. The DATA partition appears three
            // times in the mount table -- at /mnt/data and at both binds --
            // so matching on the device alone would report whichever the
            // kernel happened to list first as "the DATA tier's mount".
            let mount = spec
                .mount
                .and_then(|want| {
                    mounts
                        .iter()
                        .find(|mount| mount.device == device && mount.mount == want)
                })
                .cloned();
            let space = mount.as_ref().and_then(|mount| (self.space)(&mount.mount));
            tiers.insert(
                spec.name.to_string(),
                TierEvidence {
                    partition_bytes: partition_sizes.get(&device).copied(),
                    check: checks.get(&device).cloned(),
                    partition_label: self.partition_name(&device),
                    device: Some(device),
                    mount,
                    space,
                },
            );
        }

        let data_path = self.path("mnt/data");
        let directory_paths: Vec<_> = DATA_DIRECTORIES
            .iter()
            .map(|name| data_path.join(name))
            .filter(|path| path.symlink_metadata().is_ok_and(|meta| meta.is_dir()))
            .collect();
        let mut directory_bytes = BTreeMap::new();
        if !directory_paths.is_empty() {
            let mut command = tokio::process::Command::new("/usr/bin/du");
            command
                .args(["-s", "-x", "-B1", "--"])
                .args(&directory_paths);
            if let Some(output) = bounded_storage_output(command).await {
                for line in output.lines() {
                    if let Some((size, path)) = line.split_once('\t') {
                        for name in DATA_DIRECTORIES {
                            if std::path::Path::new(path) == data_path.join(name)
                                && let Ok(size) = size.parse::<u64>()
                            {
                                directory_bytes.insert(name.to_string(), size);
                            }
                        }
                    }
                }
            }
        }
        let project_quotas = project_quotas(&data_path);

        let binds = BINDS
            .iter()
            .map(|spec| (spec.name.to_string(), self.bind(spec, &mounts)))
            .collect();
        Ok(StorageEvidence {
            directory_bytes,
            project_quotas,
            tiers,
            media,
            binds,
        })
    }
}
