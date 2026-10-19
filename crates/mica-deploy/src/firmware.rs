//! Independent signed firmware maintenance manifests. No deployment can carry one.
//!
//! A manifest is read in two steps. [`parse_firmware`] holds it to its own
//! shape, with no knowledge of any board; [`admit_firmware`] then holds it to
//! the device, whose board facts come from the signed boot policy: the manifest
//! must name this device's board and architecture and carry exactly the target
//! the policy names. The write range is the policy's, never a table here.
use crate::board::BoardFacts;
use crate::components::{Artifact, BootIdentity, authenticate_payload, component_id};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// The most bytes an EFI loader may carry.
pub const EFI_MAX_BYTES: u64 = 4 * 1048576;
/// The most bytes a raw loader range may span: a ceiling on the arithmetic,
/// not a board's range, which the signed boot policy states exactly.
pub const LOADER_MAX_BYTES: u64 = 64 * 1048576;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Firmware {
    pub schema: String,
    pub id: String,
    pub board: String,
    pub arch: String,
    pub generation: u64,
    pub version: String,
    pub artifact: Artifact,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "format",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Target {
    /// A file on the EFI system partition.
    Efi { partition: u8, path: String },
    /// Bytes at an absolute offset on the boot disk: a loader a boot ROM
    /// reads from a fixed sector, wherever the board's ROM looks.
    DiskRange { disk_offset: u64, max_bytes: u64 },
    /// A payload inside an eMMC hardware boot area, after `payload_offset`
    /// bytes the vendor's tooling owns.
    EmmcBoot {
        area: EmmcArea,
        payload_offset: u64,
        max_bytes: u64,
    },
    /// A loader at the start of a raw NAND MTD partition, read back past bad
    /// erase blocks and never written: the recovery package replaces it.
    MtdLoader { partition: String, max_bytes: u64 },
}

/// An MTD partition name a policy may bind: 1 to 63 of `A-Z a-z 0-9 . _ -`.
#[must_use]
pub fn valid_mtd_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The eMMC hardware boot area a payload is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EmmcArea {
    Boot0,
    Boot1,
}

impl EmmcArea {
    /// The suffix the kernel gives the area's block device.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Boot0 => "boot0",
            Self::Boot1 => "boot1",
        }
    }
}

impl Target {
    /// Hold the target to its own shape, and answer the most bytes an
    /// artifact for it may carry. No board is consulted: where a board's
    /// loader goes is its signed policy's to say (`board::BoardFacts`).
    ///
    /// # Errors
    ///
    /// A path outside the ESP's loader tree, a misaligned or unbounded range.
    pub fn validate(&self) -> Result<u64> {
        match self {
            Self::Efi { partition, path } => {
                ensure!(*partition >= 1 && efi_path(path), "invalid EFI destination");
                Ok(EFI_MAX_BYTES)
            }
            Self::DiskRange {
                disk_offset,
                max_bytes,
            } => {
                ensure!(
                    *disk_offset >= FIRST_USABLE_BYTE
                        && disk_offset % 512 == 0
                        && (1..=LOADER_MAX_BYTES).contains(max_bytes)
                        && disk_offset.checked_add(*max_bytes).is_some(),
                    "invalid loader write range"
                );
                Ok(*max_bytes)
            }
            Self::EmmcBoot {
                payload_offset,
                max_bytes,
                ..
            } => {
                ensure!(
                    (1..=LOADER_MAX_BYTES).contains(max_bytes)
                        && payload_offset
                            .checked_add(*max_bytes)
                            .is_some_and(|end| end <= LOADER_MAX_BYTES),
                    "invalid eMMC boot payload"
                );
                Ok(*max_bytes)
            }
            Self::MtdLoader {
                partition,
                max_bytes,
            } => {
                ensure!(
                    valid_mtd_name(partition) && (1..=LOADER_MAX_BYTES).contains(max_bytes),
                    "invalid MTD loader"
                );
                Ok(*max_bytes)
            }
        }
    }
}

/// The first byte after the primary GPT (the protective MBR, the header and
/// 32 sectors of entries): nothing a loader writes may reach below it.
pub const FIRST_USABLE_BYTE: u64 = 34 * 512;

/// `EFI/<dir>/.../<name>.EFI`: a relative path in the ESP's loader tree, of
/// plain components, no longer than a FAT path a firmware walks.
fn efi_path(path: &str) -> bool {
    path.len() <= 128
        && path.starts_with("EFI/")
        && path.to_ascii_uppercase().ends_with(".EFI")
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

pub fn parse_firmware(payload: &[u8]) -> Result<Firmware> {
    ensure!(payload.len() <= 4096, "firmware manifest exceeds limit");
    let firmware: Firmware = serde_json::from_slice(payload)?;
    let value = serde_json::to_value(&firmware)?;
    ensure!(
        serde_json::to_vec(&value)? == payload,
        "noncanonical firmware manifest"
    );
    ensure!(
        firmware.schema == "mica/firmware/v1",
        "unsupported firmware schema"
    );
    ensure!(
        !firmware.board.is_empty()
            && firmware.board.len() <= 128
            && firmware
                .board
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid firmware board"
    );
    ensure!(
        matches!(firmware.arch.as_str(), "amd64" | "arm64" | "armhf"),
        "firmware architecture mismatch"
    );
    ensure!(
        (1..=9_007_199_254_740_991).contains(&firmware.generation),
        "invalid firmware generation"
    );
    ensure!(
        !firmware.version.is_empty()
            && firmware.version.len() <= 128
            && firmware.version.as_bytes()[0].is_ascii_alphanumeric()
            && firmware
                .version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "invalid firmware version"
    );
    ensure!(
        component_id(&value)? == firmware.id,
        "firmware identity mismatch"
    );
    ensure!(
        firmware.artifact.sha256.len() == 64
            && firmware
                .artifact
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid firmware digest"
    );
    let limit = firmware.target.validate()?;
    ensure!(
        (1..=limit).contains(&firmware.artifact.bytes),
        "invalid firmware length"
    );
    Ok(firmware)
}

pub fn authenticate_firmware(bytes: &[u8], keys: &[[u8; 32]]) -> Result<Firmware> {
    parse_firmware(&authenticate_payload(bytes, keys, 4096)?)
}

/// Hold a parsed manifest to the device its signed boot policy describes.
///
/// # Errors
///
/// A manifest for another board or architecture, a target other than the one
/// the policy names, or an artifact longer than that target allows.
pub fn admit_firmware(
    firmware: &Firmware,
    identity: &BootIdentity,
    board: &BoardFacts,
) -> Result<()> {
    ensure!(
        firmware.board == identity.board && firmware.arch == identity.arch,
        "firmware target differs from signed board policy"
    );
    ensure!(
        firmware.target == board.firmware,
        "firmware target differs from signed board policy"
    );
    ensure!(
        firmware.artifact.bytes <= board.firmware_limit(),
        "invalid firmware length"
    );
    Ok(())
}

/// Read back the authenticated destination without touching loader or counters.
pub fn verify_installed(manifest: &Firmware, boot: &crate::deployments::BootBackend) -> Result<()> {
    use crate::deployments::{BootBackend, read_bounded};
    use std::{fs::File, io::Read};
    let bytes = match (&manifest.target, boot) {
        (Target::Efi { path, .. }, BootBackend::Uefi { esp }) => {
            read_bounded(&esp.join(path), manifest.artifact.bytes)?
        }
        (Target::DiskRange { disk_offset, .. }, BootBackend::Fit { firmware, layout }) => {
            // The range is absolute on the disk and the device holds the boot
            // partition. Inside it, seek by the difference; before it, read
            // the whole disk. `admit_firmware` held the range to the signed
            // boot policy.
            use std::io::{Seek, SeekFrom};
            let start = layout.start_sector() * 512;
            let (device, offset) = match disk_offset.checked_sub(start) {
                Some(within) => (firmware.clone(), within),
                None => (whole_disk(firmware)?, *disk_offset),
            };
            let mut file = File::open(device)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = Vec::new();
            file.take(manifest.artifact.bytes).read_to_end(&mut bytes)?;
            bytes
        }
        (Target::EmmcBoot { area, .. }, BootBackend::Fit { .. }) => {
            return verify_emmc_payload(manifest, &emmc_boot_device(*area)?);
        }
        (Target::MtdLoader { partition, .. }, BootBackend::Ubi { .. }) => read_mtd_loader(
            std::path::Path::new("/sys/class/mtd"),
            std::path::Path::new("/dev"),
            partition,
            manifest.artifact.bytes,
        )?,
        _ => anyhow::bail!("firmware target differs from the boot backend"),
    };
    manifest.artifact.verify(&bytes)?;
    Ok(())
}

/// The first `bytes` bytes of the loader at the start of the MTD partition
/// named `partition`, read the way a NAND boot ROM reads it: erase block by
/// erase block, passing over the ones marked bad. The MTD classes are under
/// `class` and their device nodes under `dev`.
pub fn read_mtd_loader(
    class: &std::path::Path,
    dev: &std::path::Path,
    partition: &str,
    bytes: u64,
) -> Result<Vec<u8>> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    ensure!(valid_mtd_name(partition), "invalid MTD loader");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(class)? {
        let entry = entry?;
        let name = entry.file_name().into_string().unwrap_or_default();
        if name
            .strip_prefix("mtd")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read_to_string(entry.path().join("name"))?.trim_end_matches('\n')
                == partition
        {
            found.push((name, entry.path()));
        }
    }
    ensure!(
        found.len() == 1,
        "MTD partition {partition} is absent or ambiguous"
    );
    let (name, sysfs) = &found[0];
    let erase = std::fs::read_to_string(sysfs.join("erasesize"))?
        .trim()
        .parse::<u64>()?;
    let size = std::fs::read_to_string(sysfs.join("size"))?
        .trim()
        .parse::<u64>()?;
    ensure!(
        (4096..=16 << 20).contains(&erase) && size % erase == 0,
        "invalid MTD geometry"
    );
    let file = std::fs::File::open(dev.join(name))?;
    let meta = file.metadata()?;
    ensure!(
        meta.file_type().is_char_device()
            && std::fs::read_to_string(sysfs.join("dev"))?.trim()
                == format!(
                    "{}:{}",
                    rustix::fs::major(meta.rdev()),
                    rustix::fs::minor(meta.rdev())
                ),
        "MTD device node identity mismatch"
    );
    read_skipping_bad(&file, erase, size, bytes, |offset| {
        Ok(lifecycle_sys::mtd_bad_block(&file, offset)?)
    })
}

/// `bytes` bytes of `file` from its start, erase block by erase block of
/// `erase` bytes within `size`, leaving out the blocks `bad` names.
pub fn read_skipping_bad(
    file: &std::fs::File,
    erase: u64,
    size: u64,
    bytes: u64,
    bad: impl Fn(u64) -> Result<bool>,
) -> Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut data = Vec::new();
    let mut offset = 0;
    while (data.len() as u64) < bytes {
        ensure!(offset < size, "loader runs past its MTD partition");
        if !bad(offset)? {
            let take = erase.min(bytes - data.len() as u64);
            let start = data.len();
            data.resize(start + take as usize, 0);
            file.read_exact_at(&mut data[start..], offset)?;
        }
        offset += erase;
    }
    Ok(data)
}

/// Compare only the signed payload; the vendor creates the preceding boot header.
pub fn verify_emmc_payload(manifest: &Firmware, device: &std::path::Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let Target::EmmcBoot { payload_offset, .. } = manifest.target else {
        anyhow::bail!("expected eMMC boot firmware");
    };
    let mut file = std::fs::File::open(device)?;
    file.seek(SeekFrom::Start(payload_offset))?;
    let mut bytes = Vec::new();
    file.take(manifest.artifact.bytes).read_to_end(&mut bytes)?;
    manifest.artifact.verify(&bytes)?;
    Ok(())
}

/// The one eMMC's `area` device. More than one eMMC is refused rather than
/// guessed between.
fn emmc_boot_device(area: EmmcArea) -> Result<std::path::PathBuf> {
    use std::{fs, os::unix::fs::FileTypeExt, path::Path};
    let mut found = Vec::new();
    for entry in fs::read_dir("/sys/class/block")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name
            .strip_prefix("mmcblk")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            || !fs::read_to_string(entry.path().join("device/type"))
                .is_ok_and(|kind| kind.trim() == "MMC")
        {
            continue;
        }
        let device = Path::new("/dev").join(format!("{name}{}", area.suffix()));
        if device
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_block_device())
        {
            found.push(device);
        }
    }
    ensure!(found.len() == 1, "eMMC boot area absent or ambiguous");
    Ok(found.remove(0))
}

/// The whole-disk device holding the partition `partition`.
fn whole_disk(partition: &std::path::Path) -> Result<std::path::PathBuf> {
    use anyhow::Context;
    let name = partition.file_name().context("boot device name")?;
    let node = std::fs::canonicalize(std::path::Path::new("/sys/class/block").join(name))?;
    let disk = node
        .parent()
        .and_then(|parent| parent.file_name())
        .context("boot partition has no disk")?;
    Ok(std::path::Path::new("/dev").join(disk))
}
