//! A UBI device kept in a directory, for the store's tests: a volume table
//! (`table.json`, replaced atomically as the kernel's is) and one data file
//! per volume. It lives on disk so that a seeded device outlives the process
//! that seeded it, and so that the IO fault shim reaches its writes.
#![allow(dead_code)]

use anyhow::{Context, Result, bail, ensure};
use mica_deploy::ubi::{UbiDevice, Volume, VolumeKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

/// The bytes of one logical erase block.
pub const LEB: u64 = 126_976;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    name: String,
    #[serde(rename = "static")]
    is_static: bool,
    reserved_lebs: u64,
    /// The length a started volume update writes; data short of it is a
    /// volume still marked mid-update.
    update_bytes: Option<u64>,
    corrupted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Table {
    lebs: u64,
    next: u32,
    volumes: BTreeMap<u32, Entry>,
}

#[derive(Debug, Clone)]
pub struct DirUbi {
    root: PathBuf,
}

impl DirUbi {
    /// An empty device of `lebs` erase blocks in `root`.
    pub fn create(root: &Path, lebs: u64) -> Self {
        fs::create_dir_all(root).unwrap();
        let ubi = Self {
            root: root.to_path_buf(),
        };
        ubi.save(&Table {
            lebs,
            next: 0,
            volumes: BTreeMap::new(),
        })
        .unwrap();
        ubi
    }

    /// The device already in `root`.
    pub fn open_at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// The fixed volumes of a `ubi` board, records empty.
    pub fn with_fixed_volumes(root: &Path, lebs: u64) -> Self {
        let ubi = Self::create(root, lebs);
        for (name, lebs) in [
            ("system", 64),
            ("data", 64),
            ("records-a", 1),
            ("records-b", 1),
        ] {
            ubi.create(name, lebs * LEB, VolumeKind::Dynamic).unwrap();
        }
        ubi
    }

    fn table(&self) -> Result<Table> {
        Ok(serde_json::from_slice(&fs::read(
            self.root.join("table.json"),
        )?)?)
    }

    fn save(&self, table: &Table) -> Result<()> {
        mica_deploy::deployments::atomic_write(
            &self.root.join("table.json"),
            &serde_json::to_vec(table)?,
        )
    }

    fn data(&self, id: u32) -> PathBuf {
        self.root.join(format!("{id}.data"))
    }

    /// Mark volume `name` corrupted, as UBI does a static volume whose data
    /// fails its checksum.
    pub fn corrupt(&self, name: &str) {
        let mut table = self.table().unwrap();
        for entry in table.volumes.values_mut() {
            if entry.name == name {
                entry.corrupted = true;
            }
        }
        self.save(&table).unwrap();
    }

    /// Leave volume `name` marked mid-update.
    pub fn interrupt_update(&self, name: &str) {
        let mut table = self.table().unwrap();
        for entry in table.volumes.values_mut() {
            if entry.name == name {
                entry.update_bytes = Some(u64::MAX);
            }
        }
        self.save(&table).unwrap();
    }

    /// Every volume's name.
    pub fn names(&self) -> Vec<String> {
        self.volumes()
            .unwrap()
            .into_iter()
            .map(|volume| volume.name)
            .collect()
    }

    /// The bytes of volume `name`.
    pub fn read(&self, name: &str) -> Vec<u8> {
        let volume = mica_deploy::ubi::find(self, name).unwrap().unwrap();
        fs::read(self.data(volume.id)).unwrap()
    }

    /// Write `bytes` into a new static volume `name`, outside any store.
    pub fn put_static(&self, name: &str, bytes: &[u8]) -> u32 {
        let id = self
            .create(name, bytes.len() as u64, VolumeKind::Static)
            .unwrap();
        let mut table = self.table().unwrap();
        table.volumes.get_mut(&id).unwrap().update_bytes = Some(bytes.len() as u64);
        self.save(&table).unwrap();
        fs::write(self.data(id), bytes).unwrap();
        id
    }
}

impl UbiDevice for DirUbi {
    fn leb_bytes(&self) -> u64 {
        LEB
    }

    fn available_lebs(&self) -> Result<u64> {
        let table = self.table()?;
        Ok(table.lebs - table.volumes.values().map(|v| v.reserved_lebs).sum::<u64>())
    }

    fn volumes(&self) -> Result<Vec<Volume>> {
        let table = self.table()?;
        table
            .volumes
            .iter()
            .map(|(id, entry)| {
                let held = fs::metadata(self.data(*id)).map_or(0, |m| m.len());
                Ok(Volume {
                    id: *id,
                    name: entry.name.clone(),
                    kind: if entry.is_static {
                        VolumeKind::Static
                    } else {
                        VolumeKind::Dynamic
                    },
                    data_bytes: if entry.is_static {
                        held
                    } else {
                        entry.reserved_lebs * LEB
                    },
                    reserved_lebs: entry.reserved_lebs,
                    corrupted: entry.corrupted,
                    updating: entry.update_bytes.is_some_and(|bytes| bytes != held),
                })
            })
            .collect()
    }

    fn create(&self, name: &str, bytes: u64, kind: VolumeKind) -> Result<u32> {
        let mut table = self.table()?;
        ensure!(
            !table.volumes.values().any(|v| v.name == name),
            "volume exists"
        );
        let lebs = bytes.div_ceil(LEB);
        ensure!(
            lebs <= table.lebs - table.volumes.values().map(|v| v.reserved_lebs).sum::<u64>(),
            "no space"
        );
        let id = table.next;
        table.next += 1;
        File::create(self.data(id))?;
        table.volumes.insert(
            id,
            Entry {
                name: name.into(),
                is_static: kind == VolumeKind::Static,
                reserved_lebs: lebs,
                update_bytes: None,
                corrupted: false,
            },
        );
        self.save(&table)?;
        Ok(id)
    }

    fn remove(&self, id: u32) -> Result<()> {
        let mut table = self.table()?;
        table.volumes.remove(&id).context("no such volume")?;
        self.save(&table)?;
        fs::remove_file(self.data(id))?;
        Ok(())
    }

    fn rename(&self, renames: &[(u32, &str)]) -> Result<()> {
        let mut table = self.table()?;
        for (id, name) in renames {
            if table
                .volumes
                .iter()
                .any(|(other, v)| other != id && v.name == *name)
            {
                bail!("volume exists");
            }
            table.volumes.get_mut(id).context("no such volume")?.name = (*name).to_owned();
        }
        self.save(&table)
    }

    fn open(&self, id: u32, write: bool) -> Result<File> {
        ensure!(self.table()?.volumes.contains_key(&id), "no such volume");
        Ok(OpenOptions::new()
            .read(true)
            .write(write)
            .open(self.data(id))?)
    }

    fn path(&self, id: u32) -> PathBuf {
        self.data(id)
    }

    fn begin_update(&self, volume: &File, bytes: u64) -> Result<()> {
        let mut table = self.table()?;
        let id = self.id_of(volume, &table)?;
        let entry = table.volumes.get_mut(&id).unwrap();
        ensure!(bytes <= entry.reserved_lebs * LEB, "update exceeds volume");
        entry.update_bytes = Some(bytes);
        self.save(&table)?;
        volume.set_len(0)?;
        Ok(())
    }

    fn begin_leb_change(&self, volume: &File, leb: u32, bytes: u32) -> Result<()> {
        let table = self.table()?;
        let id = self.id_of(volume, &table)?;
        ensure!(
            leb == 0 && !table.volumes[&id].is_static && u64::from(bytes) <= LEB,
            "invalid LEB change"
        );
        volume.set_len(0)?;
        Ok(())
    }
}

impl DirUbi {
    fn id_of(&self, volume: &File, table: &Table) -> Result<u32> {
        use std::os::unix::fs::MetadataExt;
        let inode = volume.metadata()?.ino();
        table
            .volumes
            .keys()
            .copied()
            .find(|id| fs::metadata(self.data(*id)).is_ok_and(|m| m.ino() == inode))
            .context("not a volume of this device")
    }
}
