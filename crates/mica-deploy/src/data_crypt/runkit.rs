//! The runkit's DATA: opened, or formatted at its first boot, as the
//! product's `dataEncryption` says; mapped as `mica-data` with the volume key
//! in the kernel keyring; every provider sealed off before the root starts.
//! It runs in one startup worker, so no secret leaves that process except
//! the recovery passphrase of a first format, which goes to PID 1 on the
//! worker's output and from there only to DATA.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::provider::{self, Format, KeyProvider, NoProvider, Opened};
use super::{Cipher, DATA_OFFSET, Kdf, VolumeKey, has_header, tpm2};
use crate::board::DataFormat;
use crate::boot::startup::native::format::format_data;

/// The mapping's name and its node.
pub const MAPPING: &str = "mica-data";
pub const MAPPED: &str = "/dev/mapper/mica-data";
/// The mapping that checks the kernel has the cipher: loaded, never resumed,
/// discarded at once.
const PROBE: &str = "mica-data-probe";
/// The volume key's `logon` description.
const KEY: &std::ffi::CStr = c"mica:data";
const KEY_DESCRIPTION: &str = "mica:data";
/// The recovery keyslot's argon2id time cost and parallelism; its memory is
/// the product's.
const RECOVERY_TIME: u32 = 4;
const RECOVERY_CPUS: u32 = 1;
/// The bounds of the product's recovery memory cost, in KiB.
const RECOVERY_MEMORY_KIB: std::ops::RangeInclusive<u32> = 65_536..=1_048_576;

/// boot.json `dataEncryption`: absent is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Encryption {
    pub mode: Mode,
    /// The providers, in the order their tokens are tried.
    pub key_providers: Vec<ProviderKind>,
    pub no_provider: NoProvider,
    /// `aes-xts-plain64` or `xchacha12,aes-adiantum-plain64`.
    pub cipher: String,
    /// The recovery keyslot's argon2id memory, set per board at build time.
    pub recovery_memory_kib: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Tpm2,
}

impl ProviderKind {
    /// The product's spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tpm2 => "tpm2",
        }
    }

    /// The LUKS2 token type it owns.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Tpm2 => tpm2::TOKEN,
        }
    }
}

impl Encryption {
    /// The payload cipher; the field held to the two the profile has, the
    /// provider list to one non-empty list without repeats, and the recovery
    /// memory to 64 MiB..1 GiB.
    pub fn validate(&self) -> Result<Cipher> {
        let mut seen = std::collections::BTreeSet::new();
        ensure!(
            !self.key_providers.is_empty()
                && self.key_providers.iter().all(|kind| seen.insert(*kind)),
            "dataEncryption needs providers, each once"
        );
        ensure!(
            RECOVERY_MEMORY_KIB.contains(&self.recovery_memory_kib),
            "dataEncryption recoveryMemoryKib outside 65536..=1048576"
        );
        Cipher::parse(&self.cipher).context("dataEncryption cipher outside the profile")
    }

    /// The recovery keyslot's KDF.
    #[must_use]
    pub const fn recovery(&self) -> Kdf {
        Kdf::Argon2id {
            time: RECOVERY_TIME,
            memory_kib: self.recovery_memory_kib,
            cpus: RECOVERY_CPUS,
        }
    }
}

impl Cipher {
    /// The dm-crypt table's spelling.
    #[must_use]
    pub const fn dm_spec(self) -> &'static str {
        match self {
            Self::AesXts => "aes-xts-plain64",
            Self::Adiantum => "capi:adiantum(xchacha12,aes)-plain64",
        }
    }
}

/// What the worker tells PID 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Report {
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The block device to mount; absent when locked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// DATA was formatted at this boot.
    pub formatted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cipher: Option<String>,
    pub providers: Vec<ProviderReport>,
    /// The recovery passphrase of a first format; never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
    pub seal_off_failures: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    On,
    Off,
    Locked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderReport {
    #[serde(rename = "type")]
    pub kind: String,
    pub enrolled: bool,
    pub unlocked_this_boot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Whether `device` is a DATA never formatted: no LUKS2 header and no ext4
/// superblock.
pub fn fresh(device: &File) -> Result<bool> {
    if has_header(device)? {
        return Ok(false);
    }
    let mut magic = [0u8; 2];
    match device.read_exact_at(&mut magic, 1024 + 56) {
        Ok(()) => Ok(magic != [0x53, 0xEF]),
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(true),
        Err(err) => Err(err).context("read the DATA superblock"),
    }
}

/// Open DATA at `partition` (a block device) as `encryption` says: format a
/// fresh one, map an encrypted one, and seal every provider off.
pub fn open_data(
    root: &Path,
    arch: &str,
    partition: &str,
    encryption: Option<&Encryption>,
    data_format: &DataFormat,
) -> Result<Report> {
    let metadata = fs::metadata(partition)?;
    ensure!(
        partition.starts_with("/dev/") && metadata.file_type().is_block_device(),
        "DATA is not a block device"
    );
    let device = OpenOptions::new()
        .read(true)
        .write(true)
        .open(partition)
        .context("open DATA")?;
    let cipher = encryption.map(Encryption::validate).transpose()?;
    // The providers in the product's order.
    let mut listed_providers: Vec<Box<dyn KeyProvider>> = encryption
        .map_or(&[][..], |e| &e.key_providers)
        .iter()
        .map(|kind| -> Box<dyn KeyProvider> {
            match kind {
                ProviderKind::Tpm2 => Box::new(tpm2::Tpm2::open(Path::new(tpm2::DEVICE))),
            }
        })
        .collect();
    let mut providers: Vec<&mut dyn KeyProvider> = listed_providers
        .iter_mut()
        .map(|provider| &mut **provider as &mut dyn KeyProvider)
        .collect();
    let format = match (encryption, cipher) {
        (Some(encryption), Some(cipher)) if fresh(&device)? => Some(Format {
            cipher,
            no_provider: encryption.no_provider,
            recovery: encryption.recovery(),
            cipher_ready: cipher_ready(&device, cipher).map_err(|err| format!("{err:#}")),
        }),
        _ => None,
    };
    let outcome = provider::open(&device, &mut providers, format.as_ref());
    let seal_off_failures = outcome
        .seal_off_failures
        .iter()
        .map(|(kind, why)| format!("{kind}: {why}"))
        .collect();
    let report = |state, reason: Option<&str>, device: Option<&str>| Report {
        state,
        reason: reason.map(str::to_string),
        device: device.map(str::to_string),
        formatted: false,
        cipher: None,
        providers: Vec::new(),
        recovery: None,
        seal_off_failures: Vec::new(),
    };
    let listed = |failures: &provider::Failures, enrolled: &[&str], unlocked: Option<&str>| {
        encryption
            .map_or(&[][..], |e| &e.key_providers)
            .iter()
            .map(|kind| {
                let token = kind.token();
                ProviderReport {
                    kind: kind.name().into(),
                    enrolled: enrolled.contains(&token),
                    unlocked_this_boot: unlocked == Some(token),
                    failure: failures
                        .iter()
                        .rev()
                        .find(|(failed, _)| *failed == token)
                        .map(|(_, why)| why.clone()),
                }
            })
            .collect::<Vec<_>>()
    };
    let mut done = match outcome.opened? {
        Opened::Plain => report(State::Off, None, Some(partition)),
        Opened::PlainFallback { failures } => {
            drop(device);
            format_data(root, arch, partition, data_format)?;
            Report {
                formatted: true,
                providers: listed(&failures, &[], None),
                ..report(State::Off, Some("no-provider-fallback"), Some(partition))
            }
        }
        Opened::CipherUnavailable { why } => Report {
            providers: listed(&Vec::new(), &[], None),
            ..report(
                State::Locked,
                Some(&format!("cipher-unavailable: {why}")),
                None,
            )
        },
        Opened::NoProvider { failures } => Report {
            providers: listed(&failures, &[], None),
            ..report(State::Locked, Some("no-provider"), None)
        },
        Opened::Locked { failures } => Report {
            providers: listed(&failures, &[], None),
            ..report(State::Locked, Some("unseal"), None)
        },
        Opened::Unlocked {
            volume,
            key,
            provider,
        } => {
            map(&device, partition, volume.cipher(), volume.uuid(), &key)?;
            Report {
                cipher: Some(volume.cipher().spec().into()),
                providers: listed(&Vec::new(), &[], Some(provider)),
                ..report(State::On, None, Some(MAPPED))
            }
        }
        Opened::Formatted {
            volume,
            key,
            enrolled,
            recovery,
            failures,
        } => {
            map(&device, partition, volume.cipher(), volume.uuid(), &key)?;
            drop(key);
            // A new key makes the old contents unreadable: no erase first.
            let inside = DataFormat {
                secure_erase: false,
                ..data_format.clone()
            };
            format_data(root, arch, MAPPED, &inside)?;
            Report {
                formatted: true,
                cipher: Some(volume.cipher().spec().into()),
                providers: listed(&failures, &enrolled, None),
                recovery: Some(String::from_utf8(recovery.to_vec()).context("recovery")?),
                ..report(State::On, None, Some(MAPPED))
            }
        }
    };
    done.seal_off_failures = seal_off_failures;
    Ok(done)
}

/// The crypt table of DATA at `major:minor`, `bytes` long.
pub fn table(
    cipher: Cipher,
    major: u32,
    minor: u32,
    bytes: u64,
) -> Result<lifecycle_sys::DmTarget> {
    // 4096-byte sectors: the payload length is whole sectors of them.
    let length = bytes
        .checked_sub(DATA_OFFSET)
        .filter(|left| *left >= 4096)
        .context("DATA is smaller than its header")?
        / 4096
        * 8;
    Ok(lifecycle_sys::DmTarget {
        sector: 0,
        length,
        kind: "crypt".into(),
        parameters: format!(
            "{} :{}:logon:{KEY_DESCRIPTION} 0 {major}:{minor} {} 2 allow_discards sector_size:4096",
            cipher.dm_spec(),
            cipher.key_size(),
            DATA_OFFSET / 512
        ),
    })
}

/// Whether the kernel can map `device` with `cipher`: a table with a
/// throwaway key is loaded, and the mapping discarded without resuming it.
fn cipher_ready(device: &File, cipher: Cipher) -> Result<()> {
    let rdev = device.metadata()?.rdev();
    let bytes = (&*device).seek(SeekFrom::End(0))?;
    let target = table(
        cipher,
        rustix::fs::major(rdev),
        rustix::fs::minor(rdev),
        bytes,
    )?;
    let key = super::random(cipher.key_size())?;
    let serial = lifecycle_sys::add_logon_key(lifecycle_sys::PROCESS_KEYRING, KEY, &key)
        .context("put a probe key in the keyring")?;
    let checked = (|| -> Result<()> {
        let control = crate::boot::startup::verity::control()?;
        let tag: String = super::random(8)?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let created = lifecycle_sys::dm_create(&control, PROBE, &format!("MICA-PROBE-{tag}"))
            .context("create the cipher probe")?;
        let loaded = lifecycle_sys::dm_load_crypt(&control, &created, &target);
        lifecycle_sys::dm_discard_created(&control, &created)
            .context("discard the cipher probe")?;
        loaded.with_context(|| format!("the kernel cannot map {}", cipher.dm_spec()))?;
        Ok(())
    })();
    let invalidated = lifecycle_sys::invalidate_key(serial).context("invalidate the probe key");
    checked.and(invalidated)
}

/// Map `device` as [`MAPPING`]: the key in this worker's keyring for the
/// table load, invalidated once the table holds it.
fn map(device: &File, partition: &str, cipher: Cipher, uuid: &str, key: &VolumeKey) -> Result<()> {
    let rdev = device.metadata()?.rdev();
    let bytes = (&*device).seek(SeekFrom::End(0))?;
    let expected = table(
        cipher,
        rustix::fs::major(rdev),
        rustix::fs::minor(rdev),
        bytes,
    )?;
    ensure!(
        fs::metadata(partition)?.rdev() == rdev,
        "DATA changed while it was opened"
    );
    let serial = lifecycle_sys::add_logon_key(lifecycle_sys::PROCESS_KEYRING, KEY, key)
        .context("put the DATA key in the keyring")?;
    let control = crate::boot::startup::verity::control()?;
    let dm_uuid = format!("CRYPT-LUKS2-{}-{MAPPING}", uuid.replace('-', ""));
    let created = lifecycle_sys::dm_create(&control, MAPPING, &dm_uuid);
    let activated = created
        .as_ref()
        .map_err(|e| anyhow::anyhow!("create the DATA mapping: {e}"))
        .and_then(|created| {
            lifecycle_sys::dm_load_crypt(&control, created, &expected)
                .context("DATA crypt table load refused")?;
            let status = lifecycle_sys::dm_resume_writable(&control, created)
                .context("resume the DATA mapping")?;
            ensure!(
                lifecycle_sys::dm_table(&control, &status)? == [expected.clone()],
                "the kernel's DATA table differs from the one loaded"
            );
            let node = format!("/dev/dm-{}", rustix::fs::minor(created.device));
            let actual = fs::symlink_metadata(&node)?;
            ensure!(
                actual.file_type().is_block_device() && actual.rdev() == created.device,
                "new DM node identity mismatch"
            );
            std::os::unix::fs::symlink(node, MAPPED)?;
            Ok(())
        });
    // The table holds the key now; the keyring keeps none.
    let invalidated = lifecycle_sys::invalidate_key(serial).context("invalidate the DATA key");
    let finished = activated.and(invalidated);
    if finished.is_err()
        && let Ok(created) = &created
    {
        lifecycle_sys::dm_discard_created(&control, created)
            .context("rollback of the new DATA mapping failed")?;
    }
    finished
}

#[cfg(test)]
mod tests;
