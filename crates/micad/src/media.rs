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
//!
//! Each inventory read also reconciles MICA's mounts, under one lifecycle
//! lock: a supported, clean volume nothing else uses is mounted read-only at
//! [`MEDIA_ROOT`]`/<id>` (`ro,nosuid,nodev,noexec`, and `noload` for ext, so
//! no journal is replayed); a volume whose filesystem says it needs repair
//! is left alone; a volume the user unmounted stays unmounted while it is
//! plugged in; and the mount of a device that is gone is detached. Nothing
//! is ever checked or written.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use aws_lc_rs::digest::{SHA256, digest};
use serde::Serialize;

mod mount;
mod probe;
#[cfg(test)]
mod tests;

pub use mount::{HostMounter, Mounter};
pub use probe::{Probed, probe};

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
    /// `mounted_readonly`, `unmounted` (by the user, until it is plugged
    /// again), `detected` (supported but not mounted: something else uses
    /// it), `unsupported`, `failed`, or `removed` (unplugged since the last
    /// read, listed once).
    pub state: &'static str,
    /// Whether something outside MICA's media mounts uses it.
    pub busy: bool,
    /// Why the state is what it is, when it is not `detected`, or why it is
    /// busy.
    pub reason: Option<&'static str>,
}

/// What unmounting one volume came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Unmount {
    /// Unmounted, or not mounted by MICA; it stays unmounted while plugged.
    Unmounted {
        /// The volume as it is now.
        item: Volume,
    },
    /// No volume has this id now (unplugged, or an id of another plug).
    NotFound,
    /// The kernel refused: something has a file open on it.
    Busy {
        /// The volume, still mounted.
        item: Volume,
    },
}

/// The media inventory and MICA's mounts of it.
#[async_trait::async_trait]
pub trait MediaSource: Send + Sync {
    /// The media plugged in now, MICA's mounts reconciled with it.
    ///
    /// # Errors
    ///
    /// Returns an error when this daemon observes no media, or the host's
    /// tables cannot be read.
    async fn inventory(&self) -> Result<Inventory>;

    /// Unmount the volume `id` MICA mounted, without forcing: this volume
    /// only, its device and its other volumes untouched.
    ///
    /// # Errors
    ///
    /// As [`Self::inventory`], or an unmount error other than busy.
    async fn unmount(&self, id: &str) -> Result<Unmount>;
}

/// The source a daemon without host access has: none.
pub struct UnavailableMedia;

#[async_trait::async_trait]
impl MediaSource for UnavailableMedia {
    async fn inventory(&self) -> Result<Inventory> {
        Err(anyhow::anyhow!("this daemon observes no media"))
    }

    async fn unmount(&self, _id: &str) -> Result<Unmount> {
        Err(anyhow::anyhow!("this daemon observes no media"))
    }
}

/// The media of the host under `root` (`/` on a device).
pub struct HostMedia {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    mounter: Box<dyn Mounter>,
    /// The lifecycle lock, and what it keeps between reads.
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Volumes the user unmounted, while plugged in.
    unmounted: BTreeSet<String>,
    /// Volumes whose mount failed, and why, while plugged in.
    failed: BTreeMap<String, &'static str>,
    /// The volumes of the last read, to list once what has gone since.
    last: Vec<Volume>,
}

/// A volume as one scan found it.
struct Found {
    volume: Volume,
    /// Its kernel name.
    name: String,
    /// Whether it is mounted at its place under [`MEDIA_ROOT`].
    mounted: bool,
}

struct Scan {
    found: Vec<Found>,
    /// MICA's media mounts whose volume is gone.
    stale: Vec<String>,
}

#[async_trait::async_trait]
impl MediaSource for HostMedia {
    async fn inventory(&self) -> Result<Inventory> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || inner.reconcile())
            .await
            .context("media scan")?
    }

    async fn unmount(&self, id: &str) -> Result<Unmount> {
        let (inner, id) = (Arc::clone(&self.inner), id.to_string());
        tokio::task::spawn_blocking(move || inner.unmount(&id))
            .await
            .context("media unmount")?
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
        Self::at("/", Box::new(HostMounter))
    }

    /// A source over the host tree at `root`, mounting with `mounter`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>, mounter: Box<dyn Mounter>) -> Self {
        Self {
            inner: Arc::new(Inner {
                root: root.into(),
                mounter,
                state: Mutex::new(State::default()),
            }),
        }
    }
}

impl Inner {
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

    /// Where MICA mounts the volume `id`.
    fn place(&self, id: &str) -> PathBuf {
        self.path(MEDIA_ROOT.trim_start_matches('/')).join(id)
    }

    fn scan(&self) -> Result<Scan> {
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
        let mut found = Vec::new();
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
                let id = opaque("mica-media-volume", &[&boot, &seq, &number.to_string()]);
                let class = format!("sys/class/block/{name}");
                let size = self
                    .read(&format!("{class}/size"))
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    * 512;
                let devnum = self.read(&format!("{class}/dev")).unwrap_or_default();
                let place = Path::new(MEDIA_ROOT).join(&id);
                let mut mounted = false;
                let mut busy = false;
                // A mount at another place under MEDIA_ROOT is MICA's, of a
                // plug that is gone (a new plug can reuse its device number):
                // it is detached below, and uses nothing of this one.
                for mount in mounts.iter().filter(|m| m.device == devnum) {
                    let point = Path::new(&mount.point);
                    if point == place {
                        mounted = true;
                    } else if point.parent() != Some(Path::new(MEDIA_ROOT)) {
                        busy = true;
                    }
                }
                let probed = File::open(self.path(&format!("dev/{name}")))
                    .and_then(|file| probe(|offset, buf| file.read_exact_at(buf, offset)));
                let (filesystem, label, state, reason) = match probed {
                    Err(_) => ("unknown", None, "failed", Some("unreadable")),
                    Ok(Probed {
                        filesystem,
                        label,
                        dirty,
                    }) => {
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
                            Some(_) if dirty && !mounted => {
                                (filesystem, label, "failed", Some("needs_repair"))
                            }
                            Some(_) => (filesystem, label, "detected", None),
                        }
                    }
                };
                found.push(Found {
                    volume: Volume {
                        id,
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
                    },
                    name,
                    mounted,
                });
            }
        }
        let live: BTreeSet<PathBuf> = found
            .iter()
            .map(|f| Path::new(MEDIA_ROOT).join(&f.volume.id))
            .collect();
        let stale = mounts
            .iter()
            .filter(|m| {
                let point = Path::new(&m.point);
                point.parent() == Some(Path::new(MEDIA_ROOT)) && !live.contains(point)
            })
            .map(|m| m.point.clone())
            .collect();
        Ok(Scan { found, stale })
    }

    /// Mount `found` read-only at its place.
    fn mount_one(&self, found: &Found) -> Result<()> {
        let place = self.place(&found.volume.id);
        std::fs::create_dir_all(&place).context("make the mount point")?;
        let mounted = self.mounter.mount(
            &self.path(&format!("dev/{}", found.name)),
            &place,
            found.volume.filesystem,
        );
        if mounted.is_err() {
            let _ = std::fs::remove_dir(&place);
        }
        mounted.context("mount")
    }

    /// The inventory, with MICA's mounts made to match it.
    fn reconcile(&self) -> Result<Inventory> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scan = self.scan()?;
        for point in &scan.stale {
            let place = self.path(point.trim_start_matches('/'));
            match self.mounter.unmount(&place, true) {
                Ok(()) => {
                    let _ = std::fs::remove_dir(&place);
                }
                Err(error) => {
                    tracing::warn!(point, %error, "media: the mount of a removed volume stays")
                }
            }
        }
        let mut items = Vec::new();
        for found in scan.found {
            let mut volume = found.volume.clone();
            if found.mounted {
                volume.state = "mounted_readonly";
            } else if state.unmounted.contains(&volume.id) {
                volume.state = "unmounted";
            } else if let Some(reason) = state.failed.get(&volume.id) {
                volume.state = "failed";
                volume.reason = Some(reason);
            } else if volume.state == "detected" && !volume.busy {
                match self.mount_one(&found) {
                    Ok(()) => volume.state = "mounted_readonly",
                    Err(error) => {
                        tracing::warn!(id = volume.id, error = %format!("{error:#}"), "media: mount failed");
                        state.failed.insert(volume.id.clone(), "mount_failed");
                        volume.state = "failed";
                        volume.reason = Some("mount_failed");
                    }
                }
            }
            if volume.state == "mounted_readonly" {
                volume.available_bytes = self.mounter.available(&self.place(&volume.id));
            }
            items.push(volume);
        }
        let now: BTreeSet<String> = items.iter().map(|v| v.id.clone()).collect();
        state.unmounted.retain(|id| now.contains(id));
        state.failed.retain(|id, _| now.contains(id));
        let removed: Vec<Volume> = state
            .last
            .iter()
            .filter(|v| !now.contains(&v.id))
            .map(|v| Volume {
                available_bytes: None,
                state: "removed",
                busy: false,
                reason: None,
                ..v.clone()
            })
            .collect();
        state.last = items.clone();
        items.extend(removed);
        let generation =
            hex::encode(&digest(&SHA256, serde_json::to_string(&items)?.as_bytes()).as_ref()[..8]);
        Ok(Inventory { generation, items })
    }

    fn unmount(&self, id: &str) -> Result<Unmount> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scan = self.scan()?;
        let Some(found) = scan.found.into_iter().find(|f| f.volume.id == id) else {
            return Ok(Unmount::NotFound);
        };
        let mut volume = found.volume;
        if found.mounted {
            let place = self.place(id);
            if let Err(error) = self.mounter.unmount(&place, false) {
                if error.raw_os_error() == Some(rustix::io::Errno::BUSY.raw_os_error()) {
                    volume.state = "mounted_readonly";
                    volume.available_bytes = self.mounter.available(&place);
                    return Ok(Unmount::Busy { item: volume });
                }
                return Err(error).context("unmount");
            }
            let _ = std::fs::remove_dir(&place);
        }
        state.unmounted.insert(id.to_string());
        volume.state = "unmounted";
        Ok(Unmount::Unmounted { item: volume })
    }
}
