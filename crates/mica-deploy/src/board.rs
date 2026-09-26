//! What the device is, as its signed boot policy states it.
//!
//! The kernel component's initramfs carries `/etc/mica/boot.json`, and its
//! `board` section is the one place the device learns its boot backend, its
//! kernel format, the GPT numbers of the partitions it binds, the firmware it
//! may write and where the FIT boot records live. Nothing here is keyed on a
//! board's name: a new board is a new policy, not a new build of this crate.
//!
//! The policy is inside the signed UKI or FIT, so every value is authenticated.
//! `fit_env`'s rule stands -- disk contents never choose writable offsets -- and
//! the partition the records live in is held to this geometry before anything
//! is written (`deployments::boot_partition`).

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::boot::BootKind;
use crate::firmware::Target;
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

/// The `board` section of the signed boot policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BoardFacts {
    pub boot: BootKind,
    pub kernel: KernelFormat,
    pub partitions: Partitions,
    /// Exactly the `target` a firmware receipt for this board carries.
    pub firmware: Target,
    /// Present exactly when `boot` is `uboot-fit`: where the two boot record
    /// copies live inside the boot partition, and that partition's geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<FitLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<Watchdog>,
}

impl BoardFacts {
    /// Hold the section to itself and to the architecture beside it.
    ///
    /// # Errors
    ///
    /// Names the first disagreement: backend and kernel format, backend and
    /// firmware format, records on the wrong backend, partition numbers, or a
    /// firmware range that leaves its partition or reaches the records.
    pub fn validate(&self, arch: &str) -> Result<()> {
        ensure!(
            matches!(arch, "amd64" | "arm64"),
            "unsupported architecture"
        );
        ensure!(
            matches!(
                (self.boot, self.kernel),
                (BootKind::Uefi, KernelFormat::Uki) | (BootKind::UbootFit, KernelFormat::Fit)
            ),
            "boot backend and kernel format disagree"
        );
        let p = self.partitions;
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
        self.firmware.validate()?;
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
        Ok(())
    }

    /// The most bytes a firmware artifact for this board may carry.
    #[must_use]
    pub fn firmware_limit(&self) -> u64 {
        self.firmware.validate().unwrap_or(0)
    }
}
