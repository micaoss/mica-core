//! External media: the USB mass storage and removable SD cards plugged into
//! the device, as one inventory of volumes grouped by physical attachment.
//!
//! The inventory is read from sysfs and mountinfo at call time; nothing here
//! listens for hotplug, so it is the same on a systemd and an OpenRC root.
//! A disk counts as media when it sits behind a USB interface, or is an SD
//! card (an MMC card of type `SD`; soldered eMMC reports `MMC`). The disks
//! holding SYSTEM, DATA or the boot partition, found through the devices
//! actually mounted there and never through labels, are never media.
//!
//! Every partition of a media disk is a volume, supported or not; a disk with
//! no partitions is one volume. Each attachment gets opaque ids bound to the
//! kernel's `diskseq` for that attachment and this boot: unplugged and
//! plugged again, the same stick gets new ids, whatever its filesystem UUID.
//! A filesystem is recognised from its superblock, read only; its label is
//! for display and nothing else.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use aws_lc_rs::digest::{SHA256, digest};
use serde::Serialize;

mod probe;
#[cfg(test)]
mod tests;

pub use probe::probe;

/// Where MICA mounts a media volume, by its id.
pub const MEDIA_ROOT: &str = "/run/mica/media";

/// The mounts whose devices are the device's own disk.
const TIER_MOUNTS: &[&str] = &["/", "/mnt/system", "/mnt/data", "/boot"];

/// The inventory of external media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inventory {
    /// Changes whenever anything listed changes.
    pub generation: String,
    /// The volumes, grouped by device in plug order.
    pub items: Vec<Volume>,
}

/// One volume of one attached device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Volume {
    /// This volume of this attachment.
    pub id: String,
    /// The attached device it belongs to.
    pub device_id: String,
    /// The filesystem label, for display only.
    pub label: Option<String>,
    /// `usb` or `sd`.
    pub transport: &'static str,
    /// `vfat`, `exfat`, `ext2`, `ext3`, `ext4`, `ntfs`, or `unknown`.
    pub filesystem: &'static str,
    /// The volume's size.
    pub size_bytes: u64,
    /// Free space, once mounted.
    pub available_bytes: Option<u64>,
    /// `detected`, `unsupported` or `failed`.
    pub state: &'static str,
    /// Whether something outside MICA's media mounts uses it.
    pub busy: bool,
    /// Why the state is what it is, when it is not `detected`, or why it is
    /// busy.
    pub reason: Option<&'static str>,
}

/// Read-only source of the media inventory.
#[async_trait::async_trait]
pub trait MediaSource: Send + Sync {
    /// The media plugged in now.
    ///
    /// # Errors
    ///
    /// Returns an error when this daemon observes no media, or the host's
    /// tables cannot be read.
    async fn inventory(&self) -> Result<Inventory>;
}

/// The source a daemon without host access has: none.
pub struct UnavailableMedia;

#[async_trait::async_trait]
impl MediaSource for UnavailableMedia {
    async fn inventory(&self) -> Result<Inventory> {
        Err(anyhow::anyhow!("this daemon observes no media"))
    }
}

/// The media of the host under `root` (`/` on a device).
pub struct HostMedia {
    root: PathBuf,
}

#[async_trait::async_trait]
impl MediaSource for HostMedia {
    async fn inventory(&self) -> Result<Inventory> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || HostMedia { root }.scan())
            .await
            .context("media scan")?
    }
}

/// A mount, by device number.
struct Mount {
    device: String,
    point: String,
}

fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            Some(Mount {
                device: (*fields.get(2)?).to_string(),
                point: crate::storage_status::unescape_octal(fields.get(4)?),
            })
        })
        .collect()
}

/// A short opaque id: `tag`, this boot and the attachment, hashed.
fn opaque(tag: &str, parts: &[&str]) -> String {
    let text = std::iter::once(tag)
        .chain(parts.iter().copied())
        .collect::<Vec<_>>()
        .join("\n");
    hex::encode(&digest(&SHA256, text.as_bytes()).as_ref()[..10])
}

impl HostMedia {
    /// The source `main.rs` attaches on a device.
    #[must_use]
    pub fn production() -> Self {
        Self::at("/")
    }

    /// A source over the host tree at `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn read(&self, relative: &str) -> Option<String> {
        mica_fs::read_trimmed(&self.path(relative))
    }

    /// The kernel name of the block device `M:m`.
    fn name_of(&self, device: &str) -> Option<String> {
        let target = std::fs::read_link(self.path(&format!("sys/dev/block/{device}"))).ok()?;
        Some(target.file_name()?.to_str()?.to_string())
    }

    /// The whole disks under the block device `name`: its own disk for a
    /// partition, the disks under every slave for a mapping.
    fn disks_under(&self, name: &str, disks: &mut BTreeSet<String>, depth: usize) {
        if depth > 8 {
            return;
        }
        let class = self.path(&format!("sys/class/block/{name}"));
        if class.join("partition").exists() {
            if let Some(parent) = std::fs::canonicalize(&class)
                .ok()
                .and_then(|path| Some(path.parent()?.file_name()?.to_str()?.to_string()))
            {
                disks.insert(parent);
            }
            return;
        }
        let slaves: Vec<String> = std::fs::read_dir(class.join("slaves"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect();
        if slaves.is_empty() {
            disks.insert(name.to_string());
        }
        for slave in slaves {
            self.disks_under(&slave, disks, depth + 1);
        }
    }

    /// `usb` or `sd` when the disk `name` is external media.
    fn transport(&self, name: &str) -> Option<&'static str> {
        if name.starts_with("mmcblk") {
            return (self
                .read(&format!("sys/block/{name}/device/type"))
                .as_deref()
                == Some("SD"))
            .then_some("sd");
        }
        let path = std::fs::canonicalize(self.path(&format!("sys/block/{name}"))).ok()?;
        path.components()
            .any(|part| {
                part.as_os_str().to_str().is_some_and(|part| {
                    part.strip_prefix("usb").is_some_and(|bus| {
                        !bus.is_empty() && bus.bytes().all(|b| b.is_ascii_digit())
                    })
                })
            })
            .then_some("usb")
    }

    fn scan(&self) -> Result<Inventory> {
        let boot = self
            .read("proc/sys/kernel/random/boot_id")
            .context("read boot_id")?;
        let mounts = parse_mounts(
            &std::fs::read_to_string(self.path("proc/self/mountinfo")).context("read mountinfo")?,
        );
        let kernel: BTreeSet<String> = std::fs::read_to_string(self.path("proc/filesystems"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_whitespace().last().map(str::to_string))
            .collect();
        let mut own = BTreeSet::new();
        for mount in mounts
            .iter()
            .filter(|m| TIER_MOUNTS.contains(&m.point.as_str()))
        {
            if let Some(name) = self.name_of(&mount.device) {
                self.disks_under(&name, &mut own, 0);
            }
        }
        let mut disks: Vec<(u64, String, &'static str)> = std::fs::read_dir(self.path("sys/block"))
            .context("read sys/block")?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if own.contains(&name) {
                    return None;
                }
                let transport = self.transport(&name)?;
                let seq = self
                    .read(&format!("sys/block/{name}/diskseq"))?
                    .parse()
                    .ok()?;
                Some((seq, name, transport))
            })
            .collect();
        disks.sort();
        let mut items = Vec::new();
        for (seq, disk, transport) in disks {
            let seq = seq.to_string();
            let device_id = opaque("mica-media-device", &[&boot, &seq]);
            let mut parts: BTreeMap<u32, String> =
                std::fs::read_dir(self.path(&format!("sys/block/{disk}")))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_str()?.to_string();
                        let number = mica_fs::read_trimmed(&entry.path().join("partition"))?
                            .parse()
                            .ok()?;
                        Some((number, name))
                    })
                    .collect();
            if parts.is_empty() {
                parts.insert(0, disk.clone());
            }
            for (number, name) in parts {
                let class = format!("sys/class/block/{name}");
                let size = self
                    .read(&format!("{class}/size"))
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    * 512;
                let devnum = self.read(&format!("{class}/dev")).unwrap_or_default();
                let busy = mounts
                    .iter()
                    .any(|m| m.device == devnum && !Path::new(&m.point).starts_with(MEDIA_ROOT));
                let probed = File::open(self.path(&format!("dev/{name}")))
                    .and_then(|file| probe(|offset, buf| file.read_exact_at(buf, offset)));
                let (filesystem, label, state, reason) = match probed {
                    Err(_) => ("unknown", None, "failed", Some("unreadable")),
                    Ok((filesystem, label)) => {
                        let driver = match filesystem {
                            "ext2" | "ext3" | "ext4" => Some("ext4"),
                            "vfat" | "exfat" => Some(filesystem),
                            _ => None,
                        };
                        match driver {
                            None => (
                                filesystem,
                                label,
                                "unsupported",
                                Some("filesystem_unsupported"),
                            ),
                            Some(driver) if !kernel.contains(driver) => (
                                filesystem,
                                label,
                                "unsupported",
                                Some("filesystem_not_in_kernel"),
                            ),
                            Some(_) => (filesystem, label, "detected", None),
                        }
                    }
                };
                items.push(Volume {
                    id: opaque("mica-media-volume", &[&boot, &seq, &number.to_string()]),
                    device_id: device_id.clone(),
                    label,
                    transport,
                    filesystem,
                    size_bytes: size,
                    available_bytes: None,
                    state,
                    busy,
                    reason: if busy && reason.is_none() {
                        Some("mounted_outside_mica")
                    } else {
                        reason
                    },
                });
            }
        }
        let generation =
            hex::encode(&digest(&SHA256, serde_json::to_string(&items)?.as_bytes()).as_ref()[..8]);
        Ok(Inventory { generation, items })
    }
}
