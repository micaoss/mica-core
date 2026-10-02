//! Read-mostly observation of the fixed storage layout: tiers, the
//! bind namespaces, directory usage, project quotas, media health and pressure.
//!
//! The shape [`crate::network_state`] and [`crate::time_status`] take: a trait
//! with an unavailable default so a dry-run daemon or a test never inspects
//! its host, a production implementation only `main.rs` attaches, and pure
//! rendering functions the tests drive with literal evidence.
//!
//! Two rules govern every field below, because a storage surface that guesses
//! is worse than one that says nothing.

use std::collections::BTreeMap;

use serde_json::Value as Json;

mod host;
mod json;
mod parse;
mod pressure;
pub use host::*;
pub use json::*;
pub use parse::*;
pub use pressure::*;

/// One fixed tier of the layout.
pub struct TierSpec {
    /// Stable name of the tier on the wire, lowercase.
    pub name: &'static str,
    /// The GPT name the tier is looked up by when no boot policy is readable.
    /// With one, the policy's partition UUIDs and numbers find the tier and the
    /// name reported is the one on the disk.
    pub partition_label: &'static str,
    /// The tier's role in the layout.
    pub role: &'static str,
    /// Fixed mountpoint, absent for the raw firmware partition.
    pub mount: Option<&'static str>,
}

/// Three physical partitions per board. UEFI boards expose ESP; FIT boards
/// expose FIRMWARE. Immutable deployments share SYSTEM; all writable data is DATA.
pub const TIERS: &[TierSpec] = &[
    TierSpec {
        name: "esp",
        partition_label: "esp",
        role: "esp",
        mount: Some("/boot"),
    },
    TierSpec {
        name: "firmware",
        partition_label: "firmware",
        role: "firmware",
        mount: None,
    },
    TierSpec {
        name: "system",
        partition_label: "system",
        role: "ext4",
        mount: Some("/mnt/system"),
    },
    TierSpec {
        name: "data",
        partition_label: "data",
        role: "ext4",
        mount: Some(DATA_MOUNT),
    },
];

/// The signed boot policy `mica-runkit` copies into the running root, relative
/// to the observer's root.
pub const BOOT_POLICY_PATH: &str = "run/mica/boot-policy.json";

/// The partitions the signed boot policy names: by UUID for SYSTEM and DATA,
/// by GPT number on the same disk for the boot partition, which is the `esp`
/// tier on a UEFI board and the `firmware` tier on a FIT one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPartitions {
    pub boot_tier: &'static str,
    pub boot_number: u32,
    pub system_uuid: String,
    pub data_uuid: String,
}

impl PolicyPartitions {
    /// Read the fields the observer needs out of a boot policy, or `None`
    /// when any is missing. Tolerant of every other key: the policy is
    /// `mica-deploy`'s document, and this is an observer of it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let value: Json = serde_json::from_str(text).ok()?;
        let boot_tier = match value.pointer("/board/boot")?.as_str()? {
            "uefi" => "esp",
            "uboot-fit" => "firmware",
            _ => return None,
        };
        let uuid = |key: &str| {
            value
                .get(key)?
                .as_str()
                .filter(|uuid| {
                    !uuid.is_empty() && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                })
                .map(str::to_ascii_lowercase)
        };
        Some(Self {
            boot_tier,
            boot_number: u32::try_from(value.pointer("/board/partitions/boot")?.as_u64()?).ok()?,
            system_uuid: uuid("systemPartUuid")?,
            data_uuid: uuid("dataPartUuid")?,
        })
    }
}

/// Where the DATA partition itself mounts.
pub const DATA_MOUNT: &str = "/mnt/data";

/// The bind namespaces carved out of the DATA filesystem, with
/// the source each is bound from.
pub const BINDS: &[BindSpec] = &[
    BindSpec {
        name: "mica",
        mount: "/mica",
        source: "/mnt/data/mica",
        owner: "system",
    },
    BindSpec {
        name: "srv",
        mount: "/srv",
        source: "/mnt/data/srv",
        owner: "user",
    },
    BindSpec {
        name: "containers",
        mount: "/mica/containers",
        source: "/mnt/data/containers",
        owner: "system",
    },
];

/// One bind namespace.
pub struct BindSpec {
    /// Stable name on the wire.
    pub name: &'static str,
    /// Where the bind is mounted.
    pub mount: &'static str,
    /// The path under [`DATA_MOUNT`] it is bound from.
    pub source: &'static str,
    /// `system` for `/mica`, `user` for `/srv`.
    pub owner: &'static str,
}

/// The subtree the readiness probe writes into.
///
/// This directory is created by mica-data-layout inside the acquisition
/// workspace; the probe never writes to an unrelated DATA namespace.
pub const PROBE_SUBTREE: &str = "/mica/updates/staging";

/// The tier observed by the low-space policy.
///
/// One tier, one filesystem, one capacity pool -- and two namespaces on top of
/// it. Reporting `/mica` and `/srv` as if each had its own capacity would give
/// a reader two numbers that sum to twice the disk.
pub const DATA_TIER: &str = "data";
/// Used-space percentage at or above which a watched tier is `warning`.
pub const WARNING_ENTER_PERCENT: u8 = 80;
/// Used-space percentage a `warning` tier must fall BELOW to clear.
pub const WARNING_CLEAR_PERCENT: u8 = 75;
/// Used-space percentage at or above which a watched tier is `critical`.
pub const CRITICAL_ENTER_PERCENT: u8 = 90;
/// Used-space percentage a `critical` tier must fall BELOW to drop back to
/// `warning`.
pub const CRITICAL_CLEAR_PERCENT: u8 = 85;

/// One tier's space accounting, in bytes.
///
/// `free` is what an unprivileged writer can still use; `reserved` is the
/// filesystem's own reserved-blocks pool, which root can write into and an
/// application cannot. They are separate numbers because conflating them is
/// how a tier reports free space that no application can actually have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FsSpace {
    /// Filesystem size.
    pub total: u64,
    /// Space in use.
    pub used: u64,
    /// Space available to an unprivileged writer.
    pub free: u64,
    /// The filesystem's reserved pool: `total - used - free`.
    pub reserved: u64,
}

impl FsSpace {
    /// Used share of the filesystem, 0–100, rounded down.
    ///
    /// Over `total` rather than `used + free`, so the reserved pool counts as
    /// space the tier does not have left: a DATA filesystem at 100% for an
    /// application is not "95% full" because root could still write.
    #[must_use]
    pub fn used_percent(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        u8::try_from(self.used.saturating_mul(100) / self.total).unwrap_or(100)
    }
}

/// One mount, as `/proc/self/mountinfo` records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEvidence {
    /// The block device backing the mount, canonical. For a device-mapper
    /// mount this is the BACKING partition (`/sys/block/dm-N/slaves`), not
    /// `/dev/dm-N`: the booted rootfs slot is a partition under a verity
    /// device, and reporting the mapper device would leave the tier that
    /// actually holds it looking unmounted.
    pub device: String,
    /// The filesystem-relative source path from mountinfo, including bind roots.
    pub root: String,
    /// Where it is mounted.
    pub mount: String,
    /// Filesystem type.
    pub fstype: String,
    /// Whether the mount is read-only.
    pub read_only: bool,
}

/// What the system RECORDS about the last filesystem check of one device.
///
/// systemd runs `systemd-fsck@<escaped device>.service` for every fstab entry
/// with a non-zero pass number, and keeps that unit's result. This is that
/// unit, verbatim — there is no check history here beyond the last boot,
/// because the system does not keep one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckEvidence {
    /// The unit name, so an operator can go read its journal.
    pub unit: String,
    /// systemd's `ActiveState` for the unit.
    pub active_state: Option<String>,
    /// systemd's `Result`: `success`, `exit-code`, `timeout`, …
    pub result: Option<String>,
    /// The checker's exit status. fsck's own encoding: 0 clean, 1 errors
    /// were CORRECTED, 4 errors were left uncorrected.
    pub exit_status: Option<i32>,
}

/// Everything observed about one tier.
#[derive(Debug, Clone, Default)]
pub struct TierEvidence {
    /// The tier's canonical block device, when a partition with its label
    /// exists on this board.
    pub device: Option<String>,
    /// Partition size in bytes, from sysfs. Present even when the tier is
    /// not mounted, which is the only capacity number an inactive A/B slot
    /// has.
    pub partition_bytes: Option<u64>,
    /// The mount the tier's device is on, if any.
    pub mount: Option<MountEvidence>,
    /// Space accounting for a mounted tier.
    pub space: Option<FsSpace>,
    /// The last check systemd recorded for the tier's device.
    pub check: Option<CheckEvidence>,
    /// The partition's GPT name as the disk carries it, when sysfs reports it.
    pub partition_label: Option<String>,
}

/// Normalized media wear, or the reason there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaHealth {
    /// eMMC JEDEC wear registers, exported by the mmc driver under
    /// `/sys/block/<dev>/device/`.
    Emmc {
        /// Raw `life_time`, e.g. `0x01 0x02`.
        life_time_raw: String,
        /// Raw `pre_eol_info`, e.g. `0x01`.
        pre_eol_raw: Option<String>,
    },
    /// The metric exists for this class of medium but this image cannot read
    /// it. The string is the reason, and it is reported, never hidden.
    Unsupported(String),
}

/// One physical medium.
#[derive(Debug, Clone)]
pub struct MediumEvidence {
    /// Kernel name, e.g. `mmcblk0`.
    pub name: String,
    /// `mmc`, `nvme`, `scsi` or `other`, from the kernel name.
    pub kind: String,
    /// Capacity in bytes, from `/sys/block/<dev>/size` (512-byte sectors).
    pub size_bytes: Option<u64>,
    /// Vendor identity: the mmc product name, or the SCSI/NVMe model.
    pub model: Option<String>,
    /// Whether the kernel calls the medium rotational. `false` on every
    /// medium mica runs on today; recorded rather than assumed.
    pub rotational: Option<bool>,
    /// Wear/EOL, or why there is none.
    pub health: MediaHealth,
}

/// What the readiness probe did, or why it did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// A file was created, fsynced, removed and the directory fsynced.
    Passed,
    /// The write was attempted and failed; the string is the OS error.
    Failed(String),
    /// No write was attempted; the string says why.
    NotAttempted(String),
}

/// Everything observed about one bind namespace.
#[derive(Debug, Clone, Default)]
pub struct BindEvidence {
    /// The mount at the bind's mountpoint, if it is mounted at all.
    pub mount: Option<MountEvidence>,
    /// Whether the bind's source path exists under the DATA mount and is a
    /// real directory rather than a symlink. `mica-data-layout` refuses a
    /// symlink at these paths, so a symlink here is a substituted namespace.
    pub source_is_directory: Option<bool>,
    /// The readiness probe, on `/mica` only.
    pub probe: Option<ProbeOutcome>,
}

/// One bind namespace's readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Mounted, on DATA, writable.
    Ready,
    /// Mounted and on DATA, but not fully usable: read-only, or the DATA
    /// filesystem is at its critical threshold.
    Degraded,
    /// Not mounted, or mounted from something that is not the DATA
    /// partition. Either way a writer must NOT fall back to another
    /// filesystem.
    Unavailable,
    /// The observer could not establish which of the above holds.
    Unknown,
}

impl Readiness {
    /// The wire spelling the API and UI consume.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

fn mount_matches_namespace(mount: &MountEvidence) -> bool {
    BINDS.iter().any(|spec| {
        spec.mount == mount.mount
            && spec.source.strip_prefix(DATA_MOUNT) == Some(mount.root.as_str())
    })
}

/// Classify one bind namespace against the DATA tier it must live on.
///
/// Pure, and the whole of the readiness decision. Order is meaning.
#[must_use]
pub fn classify_readiness(
    bind: &BindEvidence,
    data: Option<&TierEvidence>,
    pressure: Pressure,
) -> Readiness {
    let Some(mount) = &bind.mount else {
        // A missing mount is unavailable even without a DATA tier to compare
        // against: nothing is mounted there, so nothing may be written there.
        return Readiness::Unavailable;
    };
    if bind.source_is_directory == Some(false) {
        return Readiness::Unavailable;
    }
    let Some(data) = data else {
        return Readiness::Unknown;
    };
    let Some(device) = &data.device else {
        return Readiness::Unknown;
    };
    // The mount source must resolve to the DATA partition. A bind carrying
    // any other device is a different filesystem wearing the right path.
    if &mount.device != device || !mount_matches_namespace(mount) {
        return Readiness::Unavailable;
    }
    if mount.read_only || data.mount.as_ref().is_some_and(|m| m.read_only) {
        return Readiness::Degraded;
    }
    if matches!(bind.probe, Some(ProbeOutcome::Failed(_))) {
        return Readiness::Degraded;
    }
    if pressure == Pressure::Critical {
        return Readiness::Degraded;
    }
    if bind.source_is_directory.is_none() {
        return Readiness::Unknown;
    }
    Readiness::Ready
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUsage {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub used_inodes: u64,
    pub limit_inodes: u64,
}

/// Projects 100, 101 and 102 of the filesystem `data` is the root of, or
/// `None` when any of them cannot be read.
fn project_quotas(data: &std::path::Path) -> Option<BTreeMap<u32, ProjectUsage>> {
    let root = std::fs::File::open(data).ok()?;
    [100, 101, 102]
        .into_iter()
        .map(|id| {
            Some((
                id,
                project_usage(lifecycle_sys::project_quota(&root, id).ok()?)?,
            ))
        })
        .collect()
}

fn project_usage(quota: lifecycle_sys::ProjectQuota) -> Option<ProjectUsage> {
    Some(ProjectUsage {
        used_bytes: quota.used_bytes,
        limit_bytes: quota.block_limit_kib.checked_mul(1024)?,
        used_inodes: quota.used_inodes,
        limit_inodes: quota.inode_limit,
    })
}

const DATA_DIRECTORIES: [&str; 8] = [
    "state",
    "meta",
    "mica",
    "srv",
    "cache",
    "tmp",
    "var",
    "containers",
];

/// One observation of the whole storage surface.
#[derive(Debug, Clone, Default)]
pub struct StorageEvidence {
    pub directory_bytes: BTreeMap<String, u64>,
    pub project_quotas: Option<BTreeMap<u32, ProjectUsage>>,
    /// Per-tier evidence, keyed by [`TierSpec::name`]. A tier missing from
    /// the map is a tier whose partition label names nothing on this board.
    pub tiers: BTreeMap<String, TierEvidence>,
    /// Every whole-disk medium the kernel shows.
    pub media: Vec<MediumEvidence>,
    /// Per-bind evidence, keyed by [`BindSpec::name`].
    pub binds: BTreeMap<String, BindEvidence>,
}

impl StorageEvidence {
    /// The DATA tier's evidence, when this board has one.
    #[must_use]
    pub fn data_tier(&self) -> Option<&TierEvidence> {
        self.tiers.get(DATA_TIER)
    }
}

#[cfg(test)]
mod tests;
