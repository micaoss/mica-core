//! The system as shutdown sees it: mounts, loops, mappings and who owns them.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopIdentity {
    pub device: Device,
    pub generation: u64,
    pub backing: Device,
    pub inode: u64,
    pub offset: u64,
    pub size_limit: u64,
    pub flags: u32,
}
impl LoopIdentity {
    pub(super) fn same_association(&self, other: &Self) -> bool {
        self.device == other.device
            && self.generation == other.generation
            && self.backing == other.backing
            && self.inode == other.inode
            && self.offset == other.offset
            && self.size_limit == other.size_limit
            && self.flags & !4 == other.flags & !4
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    pub device: Device,
    pub generation: u64,
    pub name: String,
    pub uuid: String,
    pub table: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub device: Device,
    pub generation: u64,
    pub name: String,
    pub holders: BTreeSet<Device>,
    pub slaves: BTreeSet<Device>,
    pub association: Option<LoopIdentity>,
    pub mapping: Option<Mapping>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ownership {
    pub deployment: String,
    /// The watchdog startup armed, as `watchdog<N>`: the shutdown PID1 feeds
    /// the same one without the boot policy in reach. Empty is `watchdog0`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub watchdog: String,
    pub backings: BTreeSet<Device>,
    pub backing_generations: Vec<(Device, u64)>,
    pub loops: Vec<LoopIdentity>,
    pub mappings: Vec<Mapping>,
    pub mounts: Vec<Mount>,
    pub allow_extra_loops: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub mounts: Vec<Mount>,
    pub blocks: Vec<Block>,
    pub processes: Vec<u32>,
    pub swaps: Vec<String>,
    pub dirty_kib: u64,
}

pub(super) fn memory_kind(kind: &str) -> bool {
    [
        "tmpfs",
        "ramfs",
        "rootfs",
        "devtmpfs",
        "proc",
        "sysfs",
        "devpts",
        "cgroup2",
        "efivarfs",
        "securityfs",
        "debugfs",
        "tracefs",
        "bpf",
        "pstore",
        "configfs",
        "mqueue",
        "hugetlbfs",
    ]
    .contains(&kind)
}
pub(super) fn protected(mount: &Mount) -> bool {
    memory_kind(&mount.kind)
        && (mount.path == "/"
            || mount.path == "/run"
            || ["/dev", "/proc", "/sys"]
                .iter()
                .any(|p| mount.path == *p || mount.path.starts_with(&format!("{p}/"))))
}

impl Snapshot {
    pub(super) fn validate(&self, owner: &Ownership) -> Result<()> {
        ensure!(
            self.mounts.len() <= 4096 && self.blocks.len() <= 4096 && self.processes.len() <= 4096,
            "excessive live graph"
        );
        ensure!(self.swaps.is_empty(), "outstanding swap users");
        ensure!(
            self.mounts
                .iter()
                .all(|m| m.propagation.iter().all(|p| p == "unbindable")),
            "mount propagation is not private"
        );
        ensure!(
            self.blocks.iter().all(|b| b
                .association
                .as_ref()
                .is_none_or(|l| owner.backings.contains(&l.backing)
                    || owner.loops.iter().any(|known| known.same_association(l)))),
            "unknown active loop backing"
        );
        ensure!(
            self.blocks.iter().all(|b| b
                .mapping
                .as_ref()
                .is_none_or(|m| owner.mappings.contains(m))
                && (b.slaves.is_empty() || b.mapping.is_some())),
            "unknown active block layer"
        );
        let root = self
            .mounts
            .iter()
            .find(|m| m.path == "/")
            .context("missing exitrd root")?;
        ensure!(
            memory_kind(&root.kind),
            "PID1 root still uses persistent storage"
        );
        for (device, generation) in &owner.backing_generations {
            if let Some(block) = self.blocks.iter().find(|b| b.device == *device) {
                ensure!(
                    block.generation == *generation,
                    "backing block device was reused"
                );
            } else {
                ensure!(
                    !self.mounts.iter().any(|m| m.device == *device)
                        && !self
                            .blocks
                            .iter()
                            .any(|b| b.association.as_ref().is_some_and(|l| l.backing == *device)),
                    "backing device disappeared while still in use"
                );
            }
        }
        for expected in &owner.loops {
            if let Some(current) = self
                .blocks
                .iter()
                .find(|b| b.device == expected.device)
                .and_then(|b| b.association.as_ref())
            {
                ensure!(
                    expected.same_association(current),
                    "reused or changed loop association"
                );
            }
        }
        if !owner.allow_extra_loops {
            ensure!(
                self.blocks
                    .iter()
                    .filter_map(|b| b.association.as_ref())
                    .all(|l| !owner.backings.contains(&l.backing)
                        || owner.loops.iter().any(|known| known.same_association(l))),
                "unknown partial-startup loop ownership"
            );
        }
        for expected in &owner.mappings {
            if let Some(block) = self.blocks.iter().find(|b| b.device == expected.device) {
                ensure!(
                    block.mapping.as_ref() == Some(expected),
                    "reused or changed MICA mapping"
                );
            }
        }
        let owned = self.owned_mounts(owner);
        ensure!(
            self.mounts
                .iter()
                .all(|m| protected(m) || owned.contains(&m.id)),
            "unknown mount ownership"
        );
        Ok(())
    }
    pub(super) fn owned_mounts(&self, owner: &Ownership) -> BTreeSet<u64> {
        let devices: BTreeSet<_> = owner
            .backings
            .iter()
            .copied()
            .chain(owner.mappings.iter().map(|m| m.device))
            .collect();
        let mut ids: BTreeSet<_> =
            self.mounts
                .iter()
                .filter(|m| {
                    devices.contains(&m.device)
                        || owner.mounts.iter().any(|old| {
                            old.id == m.id && old.device == m.device && old.root == m.root
                        })
                })
                .map(|m| m.id)
                .collect();
        for _ in 0..self.mounts.len() {
            let old = ids.len();
            for mount in &self.mounts {
                if ids.contains(&mount.parent) {
                    ids.insert(mount.id);
                }
            }
            if ids.len() == old {
                break;
            }
        }
        ids
    }
    pub(super) fn owned_loops<'a>(&'a self, owner: &Ownership) -> Vec<&'a LoopIdentity> {
        self.blocks
            .iter()
            .filter_map(|b| b.association.as_ref())
            .filter(|l| {
                owner.backings.contains(&l.backing)
                    || owner.loops.iter().any(|old| old.same_association(l))
            })
            .collect()
    }
    pub(super) fn empty(&self, owner: &Ownership) -> bool {
        self.processes.is_empty()
            && self.swaps.is_empty()
            && self.dirty_kib == 0
            && self.mounts.iter().all(protected)
            && self.owned_loops(owner).is_empty()
            && !self.blocks.iter().any(|b| {
                b.mapping
                    .as_ref()
                    .is_some_and(|m| owner.mappings.contains(m))
            })
            && !self.blocks.iter().any(|b| {
                (owner.backings.contains(&b.device)
                    || owner.loops.iter().any(|l| l.device == b.device))
                    && !b.holders.is_empty()
            })
    }
    pub(super) fn candidates(&self, owner: &Ownership) -> Vec<Operation> {
        let mut result = Vec::new();
        let loops = self.owned_loops(owner);
        for mount in &self.mounts {
            if protected(mount) || self.mounts.iter().any(|m| m.parent == mount.id) {
                continue;
            }
            if loops.iter().any(|l| l.backing == mount.device) {
                if !mount.path.starts_with("/backing/") {
                    result.push(Operation::MoveBacking(mount.clone()));
                }
            } else {
                result.push(Operation::Unmount(mount.clone()));
            }
        }
        for block in &self.blocks {
            if !block.holders.is_empty() || self.mounts.iter().any(|m| m.device == block.device) {
                continue;
            }
            if let Some(mapping) = &block.mapping
                && owner.mappings.contains(mapping)
            {
                result.push(Operation::RemoveMapping(mapping.clone()));
            }
            if let Some(association) = &block.association
                && loops.contains(&association)
            {
                result.push(Operation::DetachLoop(association.clone()));
            }
        }
        result
    }
}
