//! Releasing the running deployment's storage, step by step.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Operation {
    Scan,
    Private,
    Quiesce,
    Sync,
    SyncMount(Mount),
    Unmount(Mount),
    MoveBacking(Mount),
    RemoveMapping(Mapping),
    DetachLoop(LoopIdentity),
    Retire {
        id: String,
        /// The board section of the signed boot policy startup PID1 read:
        /// the backend, the partitions and the record geometry, carried rather
        /// than re-derived from a name.
        board: crate::board::BoardFacts,
        system: String,
        system_device: String,
        boot_device: String,
    },
}

/// Deterministic policy boundary. Production supplies only fixed typed workers.
pub trait LifecycleIo {
    fn now_ms(&self) -> u64;
    fn scan(&mut self, deadline_ms: u64) -> Result<Snapshot>;
    fn execute(&mut self, operation: &Operation, deadline_ms: u64) -> Result<()>;
    fn event(&mut self, stage: &str, detail: &str);
    fn terminal(&mut self, _action: Action, _released: Released) -> Result<()> {
        bail!("terminal action unavailable")
    }
}

pub(super) fn remaining(state: &Snapshot, owner: &Ownership) -> String {
    let mounts: Vec<_> = state.mounts.iter().filter(|m| !protected(m)).collect();
    format!(
        "mounts={} mappings={} loops={} backings={} users={} swaps={} dirtyKiB={} mountIds={:?} blockIds={:?}",
        mounts.len(),
        state.blocks.iter().filter(|b| b.mapping.is_some()).count(),
        state
            .blocks
            .iter()
            .filter(|b| b.association.is_some())
            .count(),
        owner
            .backings
            .iter()
            .filter(|dev| mounts.iter().any(|m| m.device == **dev))
            .count(),
        state.processes.len(),
        state.swaps.len(),
        state.dirty_kib,
        mounts.iter().take(8).map(|m| m.id).collect::<Vec<_>>(),
        state
            .blocks
            .iter()
            .filter(|b| b.association.is_some() || b.mapping.is_some() || !b.holders.is_empty())
            .take(8)
            .map(|b| b.device)
            .collect::<Vec<_>>()
    )
}

pub struct Released {
    pub(super) _private: (),
}

/// No requested action can be authorized by a tool status or a shell marker.
pub fn release(io: &mut impl LifecycleIo, budget: Budget, owner: &Ownership) -> Result<Released> {
    // A deployment's mappings: the root, the kernel support image and one per
    // core component.
    ensure!(
        owner.mappings.len() <= 2 + crate::components::MAX_CORE_COMPONENTS
            && owner.loops.len() <= 128
            && owner.mounts.len() <= 4096,
        "excessive ownership record"
    );
    ensure!(
        owner.backing_generations.len() == owner.backings.len()
            && owner.backings.len() <= 3
            && owner
                .backing_generations
                .iter()
                .all(|(dev, generation)| *generation > 0 && owner.backings.contains(dev))
            && owner
                .backing_generations
                .iter()
                .map(|(dev, _)| *dev)
                .collect::<BTreeSet<_>>()
                == owner.backings,
        "invalid backing ownership generations"
    );
    let mut names = BTreeSet::new();
    ensure!(
        owner
            .mappings
            .iter()
            .all(|m| crate::boot::mica_mapping(&m.name) && names.insert(&m.name)),
        "invalid owned mapping name"
    );
    io.execute(&Operation::Private, budget.operation_deadline(io.now_ms())?)?;
    io.execute(&Operation::Quiesce, budget.operation_deadline(io.now_ms())?)?;
    let initial = io.scan(budget.operation_deadline(io.now_ms())?)?;
    initial
        .validate(owner)
        .with_context(|| remaining(&initial, owner))?;
    ensure!(
        initial.processes.is_empty(),
        "userspace holders remain after quiesce"
    );
    io.event("quiesced", "users=0");
    let mut previous = None;
    for pass in 0..12 {
        let mut tried = BTreeSet::new();
        for _ in 0..512 {
            let state = io.scan(budget.operation_deadline(io.now_ms())?)?;
            state
                .validate(owner)
                .with_context(|| remaining(&state, owner))?;
            if let Some(operation) = previous.take() {
                let observed = match &operation {
                    Operation::Unmount(m) => !state.mounts.iter().any(|current| current.id == m.id),
                    Operation::MoveBacking(m) => state.mounts.iter().any(|current| {
                        current.id == m.id
                            && current.device == m.device
                            && current.path == format!("/backing/{}", m.id)
                    }),
                    Operation::RemoveMapping(m) => {
                        !state.blocks.iter().any(|b| b.device == m.device)
                    }
                    Operation::DetachLoop(l) => !state.blocks.iter().any(|b| {
                        b.device == l.device && (b.association.is_some() || !b.holders.is_empty())
                    }),
                    _ => false,
                };
                if observed {
                    io.event("release-observed", &serde_json::to_string(&operation)?);
                }
            }
            ensure!(
                state.processes.is_empty(),
                "new userspace holder during shutdown"
            );
            if state.empty(owner) {
                io.event(
                    "empty-observation",
                    "mounts=0 mappings=0 loops=0 backings=0",
                );
                io.execute(&Operation::Sync, budget.operation_deadline(io.now_ms())?)?;
                let final_state = io.scan(budget.operation_deadline(io.now_ms())?)?;
                final_state
                    .validate(owner)
                    .with_context(|| remaining(&final_state, owner))?;
                ensure!(final_state.empty(owner), "storage changed after sync");
                io.event(
                    "storage-released",
                    "observations=2 mounts=0 mappings=0 loops=0 backings=0",
                );
                return Ok(Released { _private: () });
            }
            let mut next = None;
            for operation in state.candidates(owner) {
                let key = serde_json::to_string(&operation)?;
                if tried.insert(key) {
                    next = Some(operation);
                    break;
                }
            }
            let Some(operation) = next else {
                io.event("remaining", &remaining(&state, owner));
                if state.dirty_kib > 0 {
                    io.execute(&Operation::Sync, budget.operation_deadline(io.now_ms())?)?;
                }
                break;
            };
            if let Operation::Unmount(mount) = &operation {
                io.execute(
                    &Operation::SyncMount(mount.clone()),
                    budget.operation_deadline(io.now_ms())?,
                )?;
            }
            match io.execute(&operation, budget.operation_deadline(io.now_ms())?) {
                Ok(()) => io.event("operation-returned", &serde_json::to_string(&operation)?),
                Err(error) => io.event("operation-refused", &format!("pass={pass} {error:#}")),
            }
            previous = Some(operation);
            // Always re-observe, including after refusal: tools can mutate then fail.
        }
    }
    bail!("storage-not-released after twelve passes")
}

/// A returned terminal operation is always a failure, including status zero.
pub fn finish(
    io: &mut impl LifecycleIo,
    budget: Budget,
    owner: &Ownership,
    action: Action,
) -> Result<()> {
    let released = release(io, budget, owner)?;
    io.terminal(action, released)?;
    bail!("terminal action returned")
}

/// Bounded best-effort console transport. A failed transition record prevents
/// terminal authorization; failure diagnostics never wait on a full console.
pub fn diagnostic(message: &str) -> Result<()> {
    write_diagnostic(rustix::stdio::stderr(), message)
}
pub(super) fn write_diagnostic(fd: impl std::os::fd::AsFd, message: &str) -> Result<()> {
    let fd = fd.as_fd();
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    let mut line = message.as_bytes()[..message.len().min(1023)].to_vec();
    line.push(b'\n');
    ensure!(
        rustix::io::write(fd, &line)? == line.len(),
        "incomplete lifecycle console write"
    );
    Ok(())
}
