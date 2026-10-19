//! The UBI device of a `ubi` board (`docs/mica-core.md` 6.6): its volumes,
//! and objects written to static volumes as files are written to SYSTEM.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub use lifecycle_sys::UbiVolumeKind as VolumeKind;

/// The volume holding SYSTEM's files.
pub const SYSTEM: &str = "system";
/// The volume holding DATA.
pub const DATA: &str = "data";
/// The volumes of the two boot record copies, A then B.
pub const RECORDS: [&str; 2] = ["records-a", "records-b"];
/// The prefix of an object volume while it is written.
pub const PENDING: &str = "p.";

/// One volume as the device reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub id: u32,
    pub name: String,
    pub kind: VolumeKind,
    /// The bytes of data it holds; for a static volume, its object's length.
    pub data_bytes: u64,
    /// The logical erase blocks it reserves.
    pub reserved_lebs: u64,
    /// A static volume whose data fails its checksum.
    pub corrupted: bool,
    /// A volume update was started and not finished.
    pub updating: bool,
}

impl Volume {
    /// Whether every byte of the volume can be read: neither corrupted nor
    /// marked mid-update.
    #[must_use]
    pub fn sound(&self) -> bool {
        !self.corrupted && !self.updating
    }
}

/// The operations the store performs on a UBI device. The production one is
/// [`SysUbi`]; tests substitute a device of their own.
pub trait UbiDevice: Send + Sync + std::fmt::Debug {
    /// The usable bytes of one logical erase block.
    fn leb_bytes(&self) -> u64;
    /// The logical erase blocks no volume reserves.
    fn available_lebs(&self) -> Result<u64>;
    /// Every volume.
    fn volumes(&self) -> Result<Vec<Volume>>;
    /// Create a volume of `bytes`, returning its number.
    fn create(&self, name: &str, bytes: u64, kind: VolumeKind) -> Result<u32>;
    /// Remove volume `id`.
    fn remove(&self, id: u32) -> Result<()>;
    /// Rename volumes in one volume-table update.
    fn rename(&self, renames: &[(u32, &str)]) -> Result<()>;
    /// Volume `id`'s character device, for reading or for writing.
    fn open(&self, id: u32, write: bool) -> Result<File>;
    /// The path volume `id` reads from.
    fn path(&self, id: u32) -> PathBuf;
    /// Start replacing all of `volume` (opened for writing) with `bytes`
    /// bytes, written next.
    fn begin_update(&self, volume: &File, bytes: u64) -> Result<()>;
    /// Start replacing logical erase block `leb` of `volume` (opened for
    /// writing) atomically with `bytes` bytes, written next.
    fn begin_leb_change(&self, volume: &File, leb: u32, bytes: u32) -> Result<()>;
}

/// The volume named `name`, if there is one.
pub fn find(ubi: &dyn UbiDevice, name: &str) -> Result<Option<Volume>> {
    let mut found = ubi.volumes()?.into_iter().filter(|v| v.name == name);
    let volume = found.next();
    ensure!(found.next().is_none(), "ambiguous UBI volume {name}");
    Ok(volume)
}

/// The logical erase blocks `bytes` of data occupy.
#[must_use]
pub fn lebs(ubi: &dyn UbiDevice, bytes: u64) -> u64 {
    bytes.div_ceil(ubi.leb_bytes())
}

/// Read all of `volume`, at most `limit` bytes. A volume that is not sound,
/// or whose read fails or comes up short, cannot be read whole.
pub fn read_whole(ubi: &dyn UbiDevice, volume: &Volume, limit: u64) -> Result<Vec<u8>> {
    ensure!(volume.sound(), "UBI volume {} is not sound", volume.name);
    ensure!(
        volume.data_bytes <= limit,
        "UBI volume {} exceeds its bound",
        volume.name
    );
    let mut bytes = Vec::new();
    ubi.open(volume.id, false)?
        .take(volume.data_bytes)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == volume.data_bytes,
        "UBI volume {} read short",
        volume.name
    );
    Ok(bytes)
}

/// Hold the static volume `name` to `artifact`: sound, of the signed length
/// and the signed digest.
pub fn verify_object(
    ubi: &dyn UbiDevice,
    name: &str,
    artifact: &crate::components::Artifact,
) -> Result<()> {
    let volume = find(ubi, name)?.with_context(|| format!("UBI object {name} is absent"))?;
    verify_volume(ubi, &volume, artifact)
}

fn verify_volume(
    ubi: &dyn UbiDevice,
    volume: &Volume,
    artifact: &crate::components::Artifact,
) -> Result<()> {
    ensure!(
        volume.kind == VolumeKind::Static && volume.sound() && volume.data_bytes == artifact.bytes,
        "object length or type mismatch: UBI volume {}",
        volume.name
    );
    let mut file = ubi.open(volume.id, false)?;
    let mut digest = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let mut buffer = vec![0_u8; 65536];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        ensure!(bytes <= artifact.bytes, "object grew during verification");
        digest.update(&buffer[..read]);
    }
    ensure!(
        bytes == artifact.bytes && hex::encode(digest.finish()) == artifact.sha256,
        "object digest mismatch: UBI volume {}",
        volume.name
    );
    Ok(())
}

/// Write `artifact` from `source` to the static volume `name`, as a file is
/// published: a leftover `p.<name>` is removed, `p.<name>` is created at the
/// signed length, filled by a volume update, read back against the signed
/// digest and renamed to `name`. An existing `name` is verified, not
/// rewritten. The caller holds the transaction lock.
pub fn publish_object(
    ubi: &dyn UbiDevice,
    source: &Path,
    name: &str,
    artifact: &crate::components::Artifact,
) -> Result<()> {
    if find(ubi, name)?.is_some() {
        return verify_object(ubi, name, artifact);
    }
    let pending = format!("{PENDING}{name}");
    if let Some(stale) = find(ubi, &pending)? {
        ubi.remove(stale.id)?;
    }
    let id = ubi.create(&pending, artifact.bytes, VolumeKind::Static)?;
    let mut input = File::open(source)?.take(artifact.bytes + 1);
    let mut output = ubi.open(id, true)?;
    ubi.begin_update(&output, artifact.bytes)?;
    let copied = std::io::copy(&mut input, &mut output)?;
    ensure!(copied == artifact.bytes, "object source length mismatch");
    output.flush()?;
    output.sync_all()?;
    drop(output);
    let written = find(ubi, &pending)?.context("pending UBI object vanished")?;
    verify_volume(ubi, &written, artifact)?;
    ubi.rename(&[(id, name)])
}

/// The UBI device the kernel attached from the MTD partition named `mtd`.
#[derive(Debug)]
pub struct SysUbi {
    number: u32,
    leb_bytes: u64,
    sysfs: PathBuf,
    dev: PathBuf,
}

impl SysUbi {
    /// The attached UBI device on the MTD partition `mtd`.
    pub fn attached(mtd: &str) -> Result<Self> {
        Self::under(Path::new("/sys/class"), Path::new("/dev"), mtd)
    }

    /// As [`Self::attached`], reading the sysfs classes under `class` and the
    /// device nodes under `dev`.
    pub fn under(class: &Path, dev: &Path, mtd: &str) -> Result<Self> {
        ensure!(
            crate::firmware::valid_mtd_name(mtd),
            "invalid UBI MTD partition"
        );
        let sysfs = class.join("ubi");
        let mut found = Vec::new();
        for entry in fs::read_dir(&sysfs).context("no UBI device is attached")? {
            let name = entry?.file_name().into_string().unwrap_or_default();
            let Some(number) = name
                .strip_prefix("ubi")
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let mtd_number = read_number(&sysfs.join(&name).join("mtd_num"))?;
            let mtd_name = fs::read_to_string(class.join(format!("mtd/mtd{mtd_number}/name")))?;
            if mtd_name.trim_end_matches('\n') == mtd {
                found.push(number);
            }
        }
        ensure!(
            found.len() == 1,
            "the UBI device of MTD partition {mtd} is absent or ambiguous"
        );
        let number = found[0];
        let leb_bytes = read_number(&sysfs.join(format!("ubi{number}/eraseblock_size")))?;
        ensure!(
            (4096..=4 << 20).contains(&leb_bytes),
            "invalid UBI erase block size"
        );
        Ok(Self {
            number,
            leb_bytes,
            sysfs,
            dev: dev.to_path_buf(),
        })
    }

    /// The UBI device's number: `ubiN`.
    #[must_use]
    pub fn number(&self) -> u32 {
        self.number
    }

    /// A character device under `/dev` held to the identity sysfs gives it.
    fn node(&self, name: &str, write: bool) -> Result<File> {
        let path = self.dev.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(node_flags())
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let meta = file.metadata()?;
        ensure!(
            meta.file_type().is_char_device()
                && fs::read_to_string(self.sysfs.join(name).join("dev"))?.trim()
                    == format!(
                        "{}:{}",
                        rustix::fs::major(meta.rdev()),
                        rustix::fs::minor(meta.rdev())
                    ),
            "UBI device node identity mismatch: {}",
            path.display()
        );
        Ok(file)
    }

    fn control(&self) -> Result<File> {
        self.node(&format!("ubi{}", self.number), false)
    }
}

fn node_flags() -> i32 {
    (rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW).bits() as i32
}

fn read_number(path: &Path) -> Result<u64> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let text = text.trim_end_matches('\n');
    ensure!(
        !text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid number in {}",
        path.display()
    );
    Ok(text.parse()?)
}

impl UbiDevice for SysUbi {
    fn leb_bytes(&self) -> u64 {
        self.leb_bytes
    }

    fn available_lebs(&self) -> Result<u64> {
        read_number(
            &self
                .sysfs
                .join(format!("ubi{}/avail_eraseblocks", self.number)),
        )
    }

    fn volumes(&self) -> Result<Vec<Volume>> {
        let prefix = format!("ubi{}_", self.number);
        let mut volumes = Vec::new();
        for entry in fs::read_dir(&self.sysfs)? {
            let name = entry?.file_name().into_string().unwrap_or_default();
            let Some(id) = name
                .strip_prefix(&prefix)
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let directory = self.sysfs.join(&name);
            let flag = |file: &str| -> Result<bool> {
                match read_number(&directory.join(file))? {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => bail!("invalid UBI volume flag {file}"),
                }
            };
            volumes.push(Volume {
                id,
                name: fs::read_to_string(directory.join("name"))?
                    .trim_end_matches('\n')
                    .to_owned(),
                kind: match fs::read_to_string(directory.join("type"))?.trim() {
                    "static" => VolumeKind::Static,
                    "dynamic" => VolumeKind::Dynamic,
                    _ => bail!("invalid UBI volume type"),
                },
                data_bytes: read_number(&directory.join("data_bytes"))?,
                reserved_lebs: read_number(&directory.join("reserved_ebs"))?,
                corrupted: flag("corrupted")?,
                updating: flag("upd_marker")?,
            });
            ensure!(volumes.len() <= 128, "UBI volume count exceeds bound");
        }
        volumes.sort_by_key(|v| v.id);
        Ok(volumes)
    }

    fn create(&self, name: &str, bytes: u64, kind: VolumeKind) -> Result<u32> {
        lifecycle_sys::ubi_mkvol(self.control()?, name, bytes, kind)
            .with_context(|| format!("create UBI volume {name}"))
    }

    fn remove(&self, id: u32) -> Result<()> {
        lifecycle_sys::ubi_rmvol(self.control()?, id)
            .with_context(|| format!("remove UBI volume {id}"))
    }

    fn rename(&self, renames: &[(u32, &str)]) -> Result<()> {
        lifecycle_sys::ubi_rename(self.control()?, renames).context("rename UBI volumes")
    }

    fn open(&self, id: u32, write: bool) -> Result<File> {
        self.node(&format!("ubi{}_{id}", self.number), write)
    }

    fn path(&self, id: u32) -> PathBuf {
        self.dev.join(format!("ubi{}_{id}", self.number))
    }

    fn begin_update(&self, volume: &File, bytes: u64) -> Result<()> {
        lifecycle_sys::ubi_volume_update(volume, bytes).context("start UBI volume update")
    }

    fn begin_leb_change(&self, volume: &File, leb: u32, bytes: u32) -> Result<()> {
        lifecycle_sys::ubi_leb_change(volume, leb, bytes).context("start UBI LEB change")
    }
}
