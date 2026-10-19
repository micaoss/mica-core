//! What the device is, as its signed boot policy states it.
//!
//! The kernel component's initramfs carries `/etc/mica/boot.json`, and its
//! `board` section is the one place the device learns its boot backend and
//! what it proves, its kernel format, its storage (the GPT numbers of the
//! partitions it binds, or its UBI device), the firmware it may write, where
//! the FIT boot records live and the free space it keeps. Nothing here is keyed on a
//! board's name: a new board is a new policy, not a new build of this crate.
//!
//! The policy is inside the signed UKI or FIT, so every value is authenticated.
//! `fit_env`'s rule stands -- disk contents never choose writable offsets -- and
//! the partition the records live in is held to this geometry before anything
//! is written (`deployments::boot_partition`).

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::boot::BootKind;
use crate::firmware::{Target, valid_mtd_name};
use crate::fit_env::{ENV_SIZE, FitLayout};

/// The kernel component's boot format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KernelFormat {
    Uki,
    Fit,
}

impl KernelFormat {
    /// The spelling a deployment's `kernel.boot.format` carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uki => "uki",
            Self::Fit => "fit",
        }
    }
}

/// What the boot path proves of the FIT it loads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootTrust {
    /// The loader verifies the FIT's signature.
    #[default]
    Fit,
    /// The loader checks only the FIT's hashes: the board has no verified boot.
    None,
}

impl BootTrust {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_fit(&self) -> bool {
        *self == Self::Fit
    }
}

/// Where the device keeps its deployments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Storage {
    /// A block disk with a GPT: SYSTEM and DATA partitions.
    #[default]
    Gpt,
    /// Raw NAND under UBI: the volumes of `docs/mica-core.md` 6.6.
    Ubi,
}

impl Storage {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_gpt(&self) -> bool {
        *self == Self::Gpt
    }
}

/// The UBI device of a `ubi` board.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ubi {
    /// The MTD partition holding the UBI device.
    pub mtd: String,
}

/// The GPT numbers of the three partitions the device binds: the boot medium
/// (the ESP on UEFI, the raw firmware partition on FIT), SYSTEM and DATA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Partitions {
    pub boot: u32,
    pub system: u32,
    pub data: u32,
}

/// The largest GPT partition number the device accepts: the entry count of a
/// standard partition array.
const MAX_PARTITION: u32 = 128;

/// Which watchdog the device arms, by the identity its driver reports.
///
/// Absent, the device arms `watchdog0`. A board whose kernel registers more
/// than one watchdog names the one its loader started, so the choice is the
/// board's declaration and never the kernel's probe order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Watchdog {
    /// `/sys/class/watchdog/watchdog<N>/identity`, exactly.
    pub identity: String,
}

const MIB: u64 = 1024 * 1024;

/// The free space the device keeps, in bytes, beside what it writes: on SYSTEM
/// and the ESP beside an installed deployment, on DATA beside every download.
///
/// Each is the board's declaration, so a board sized for a small part keeps a
/// few MiB; an undeclared one keeps the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reserves {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<u64>,
    /// Only on a board with an ESP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub esp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<u64>,
}

impl Reserves {
    pub const DEFAULT_SYSTEM: u64 = 128 * MIB;
    pub const DEFAULT_ESP: u64 = 64 * MIB;
    pub const DEFAULT_DATA: u64 = 128 * MIB;

    #[must_use]
    pub fn system(&self) -> u64 {
        self.system.unwrap_or(Self::DEFAULT_SYSTEM)
    }

    #[must_use]
    pub fn esp(&self) -> u64 {
        self.esp.unwrap_or(Self::DEFAULT_ESP)
    }

    #[must_use]
    pub fn data(&self) -> u64 {
        self.data.unwrap_or(Self::DEFAULT_DATA)
    }

    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn validate(&self, boot: BootKind) -> Result<()> {
        ensure!(
            self.esp.is_none() || boot == BootKind::Uefi,
            "an ESP reserve on a board with no ESP"
        );
        for (name, value) in [
            ("system", self.system),
            ("esp", self.esp),
            ("data", self.data),
        ] {
            if let Some(bytes) = value {
                ensure!(
                    (MIB..=1024 * MIB).contains(&bytes),
                    "invalid {name} reserve"
                );
            }
        }
        Ok(())
    }
}

/// The `board` section of the signed boot policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BoardFacts {
    pub boot: BootKind,
    pub kernel: KernelFormat,
    /// `gpt` unless the board keeps its deployments under UBI; only `ubi` is
    /// written.
    #[serde(default, skip_serializing_if = "Storage::is_gpt")]
    pub storage: Storage,
    /// Present exactly on a `gpt` board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<Partitions>,
    /// Present exactly on a `ubi` board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ubi: Option<Ubi>,
    /// Exactly the `target` a firmware receipt for this board carries.
    pub firmware: Target,
    /// Present exactly on a `gpt` board whose `boot` is `uboot-fit`: where the
    /// two boot record copies live inside the boot partition, and that
    /// partition's geometry. A `ubi` board's copies are its records volumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<FitLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<Watchdog>,
    #[serde(default, skip_serializing_if = "Reserves::is_default")]
    pub reserves: Reserves,
    /// Whether DATA is mounted with `prjquota` and its project directories
    /// carry limits. On unless the board turns it off; only `false` is written.
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub data_quotas: bool,
    /// `fit` unless the board's loader checks only hashes; only `none` is
    /// written.
    #[serde(default, skip_serializing_if = "BootTrust::is_fit")]
    pub boot_trust: BootTrust,
}

fn enabled() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_enabled(value: &bool) -> bool {
    *value
}

impl BoardFacts {
    /// Hold the section to itself and to the architecture beside it.
    ///
    /// # Errors
    ///
    /// Names the first disagreement: backend and kernel format, a boot trust
    /// of `none` off FIT, backend and firmware format, records on the wrong
    /// backend, partition numbers, a firmware range that leaves its partition
    /// or reaches the records, a UBI board's device, quotas or loader, or a
    /// reserve outside 1 MiB to 1 GiB or for an ESP the board does not have.
    pub fn validate(&self, arch: &str) -> Result<()> {
        ensure!(
            matches!(arch, "amd64" | "arm64" | "armhf"),
            "unsupported architecture"
        );
        ensure!(
            matches!(
                (self.boot, self.kernel),
                (BootKind::Uefi, KernelFormat::Uki) | (BootKind::UbootFit, KernelFormat::Fit)
            ),
            "boot backend and kernel format disagree"
        );
        ensure!(
            self.boot_trust.is_fit() || self.boot == BootKind::UbootFit,
            "boot trust none on a board without FIT"
        );
        self.firmware.validate()?;
        match self.storage {
            Storage::Gpt => self.validate_gpt()?,
            Storage::Ubi => self.validate_ubi()?,
        }
        if let Some(watchdog) = &self.watchdog {
            ensure!(
                (1..=32).contains(&watchdog.identity.len())
                    && watchdog
                        .identity
                        .bytes()
                        .all(|b| (b' '..=b'~').contains(&b)),
                "invalid watchdog identity"
            );
        }
        self.reserves.validate(self.boot)?;
        Ok(())
    }

    /// A `gpt` board's partitions, records and loader.
    fn validate_gpt(&self) -> Result<()> {
        ensure!(self.ubi.is_none(), "a GPT board carries no UBI device");
        let p = self
            .partitions
            .context("GPT partitions absent from the boot policy")?;
        ensure!(
            [p.boot, p.system, p.data]
                .iter()
                .all(|n| (1..=MAX_PARTITION).contains(n))
                && p.boot != p.system
                && p.boot != p.data
                && p.system != p.data,
            "invalid partition numbers"
        );
        match (self.boot, &self.records) {
            (BootKind::UbootFit, None) => bail!("FIT boot records absent from the boot policy"),
            (BootKind::Uefi, Some(_)) => bail!("a UEFI board carries no FIT boot records"),
            _ => {}
        }
        match (&self.firmware, self.boot, self.records) {
            (Target::Efi { partition, .. }, BootKind::Uefi, _) => {
                ensure!(u32::from(*partition) == p.boot, "invalid EFI destination");
            }
            (
                Target::DiskRange {
                    disk_offset,
                    max_bytes,
                },
                BootKind::UbootFit,
                Some(records),
            ) => {
                // Inside the boot partition or before it, and clear of both
                // record copies: a loader never overwrites what selects the
                // deployment it loads.
                let end = disk_offset + max_bytes;
                let partition_end = (records.start_sector() + records.sectors()) * 512;
                let clear = records.offsets().into_iter().all(|offset| {
                    let copy = records.start_sector() * 512 + offset;
                    end <= copy || *disk_offset >= copy + ENV_SIZE as u64
                });
                ensure!(end <= partition_end && clear, "invalid loader write range");
            }
            (Target::EmmcBoot { .. }, BootKind::UbootFit, _) => {}
            _ => bail!("firmware format disagrees with the boot backend"),
        }
        Ok(())
    }

    /// A `ubi` board: FIT, its UBI device, no quotas, and a loader in an MTD
    /// partition of its own.
    fn validate_ubi(&self) -> Result<()> {
        ensure!(
            self.boot == BootKind::UbootFit,
            "UBI storage on a board without FIT"
        );
        ensure!(
            self.partitions.is_none() && self.records.is_none(),
            "a UBI board carries no GPT partitions or record geometry"
        );
        let ubi = self
            .ubi
            .as_ref()
            .context("UBI device absent from the boot policy")?;
        ensure!(valid_mtd_name(&ubi.mtd), "invalid UBI MTD partition");
        ensure!(!self.data_quotas, "UBIFS has no project quotas");
        match &self.firmware {
            Target::MtdLoader { partition, .. } => {
                ensure!(*partition != ubi.mtd, "the loader shares the UBI partition");
            }
            _ => bail!("firmware format disagrees with the boot backend"),
        }
        Ok(())
    }

    /// A `gpt` board's partition numbers.
    ///
    /// # Errors
    ///
    /// On a `ubi` board, which has none.
    pub fn gpt_partitions(&self) -> Result<Partitions> {
        self.partitions
            .context("this board keeps its deployments under UBI, not on GPT partitions")
    }

    /// The most bytes a firmware artifact for this board may carry.
    #[must_use]
    pub fn firmware_limit(&self) -> u64 {
        self.firmware.validate().unwrap_or(0)
    }
}
