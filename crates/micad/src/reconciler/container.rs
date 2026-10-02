//! Reconciler for the `container` settings subtree, on either init.
//!
//! mica-podman's supervisor, mica-containerd, owns every container
//! (`crate::containerd`). This reconciler starts it -- `mica-containerd.service`
//! or the OpenRC script of the same name, through [`UnitControl`] -- and
//! declares the settings' containers to it in one call. The daemon compares,
//! recreates only what changed and brings them back at boot on its own; micad
//! runs no container.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use micad_settings::{ContainerUnit, Settings};

use super::Reconciler;
use super::systemd::{UnitControl, is_active, is_enabled};
use crate::containerd::{Containerd, SERVICE_UNIT};
use crate::containers::ContainerEngine;

/// STATE's `quadlet` directory. Nothing reads Quadlet files on a device; this
/// reconciler removes the ones named as micad's.
const LEFTOVER_QUADLET_DIR: &str = "/mnt/data/state/quadlet";
/// The prefix of every Quadlet file named as micad's.
const LEFTOVER_PREFIX: &str = "50-mica-";

/// How long the daemon may take to answer after its service started.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the daemon may take to remove every container once none is
/// declared: it removes them on its next pass, not in the call.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);
/// Between two looks while waiting.
const POLL: Duration = Duration::from_millis(500);

/// Reconciler for the `container` subtree.
pub struct ContainerReconciler<C: UnitControl> {
    control: C,
    daemon: Arc<dyn Containerd>,
    engine: Arc<dyn ContainerEngine>,
    leftovers: PathBuf,
    ready_timeout: Duration,
    drain_timeout: Duration,
    poll: Duration,
}

impl<C: UnitControl> ContainerReconciler<C> {
    /// A reconciler driving `daemon` and its service through `control`, reading
    /// `engine` to see containers go, and removing micad's Quadlet files from
    /// `leftovers`.
    pub fn new(
        control: C,
        daemon: Arc<dyn Containerd>,
        engine: Arc<dyn ContainerEngine>,
        leftovers: PathBuf,
    ) -> Self {
        Self {
            control,
            daemon,
            engine,
            leftovers,
            ready_timeout: READY_TIMEOUT,
            drain_timeout: DRAIN_TIMEOUT,
            poll: POLL,
        }
    }

    /// The installed daemon, podman and STATE.
    pub fn production(control: C) -> Self {
        Self::new(
            control,
            Arc::new(crate::containerd::Client::production()),
            Arc::new(crate::containers::Podman::default()),
            PathBuf::from(LEFTOVER_QUADLET_DIR),
        )
    }

    /// Shorter waits, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn with_waits(mut self, ready: Duration, drain: Duration, poll: Duration) -> Self {
        self.ready_timeout = ready;
        self.drain_timeout = drain;
        self.poll = poll;
        self
    }

    /// Wait until the daemon answers its status.
    async fn ready(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.ready_timeout;
        loop {
            match self.daemon.status().await {
                Ok(_) => return Ok(()),
                Err(err) if tokio::time::Instant::now() >= deadline => {
                    bail!(
                        "mica-containerd did not answer within {} seconds of starting: {err}",
                        self.ready_timeout.as_secs()
                    )
                }
                Err(_) => tokio::time::sleep(self.poll).await,
            }
        }
    }

    /// Remove the Quadlet files named as micad's: nothing reads them. A file
    /// an integrator wrote is left alone.
    fn remove_leftovers(&self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.leftovers) else {
            return Ok(removed);
        };
        for entry in entries.flatten() {
            let Some(file) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if file.starts_with(LEFTOVER_PREFIX) && file.ends_with(".container") {
                std::fs::remove_file(entry.path())
                    .with_context(|| format!("remove the Quadlet file {file}"))?;
                removed.push(file);
            }
        }
        removed.sort();
        Ok(removed)
    }

    async fn turn_on(
        &self,
        declared: &BTreeMap<String, ContainerUnit>,
    ) -> Result<serde_json::Value> {
        if !is_enabled(&self.control.unit_file_state(SERVICE_UNIT).await?) {
            self.control.enable(SERVICE_UNIT).await?;
        }
        if !is_active(&self.control.active_state(SERVICE_UNIT).await?) {
            self.control.start(SERVICE_UNIT).await?;
        }
        self.ready().await?;
        // One call per pass, idempotent: the daemon compares and recreates only
        // what changed, and writes nothing when nothing did.
        let answer = self
            .daemon
            .declare(declared)
            .await
            .context("declare the containers to mica-containerd")?;
        Ok(answer)
    }

    /// Remove every container, wait until they are gone, then stop the daemon.
    ///
    /// The order is the point: the daemon removes an undeclared container on
    /// its next pass, not in the call, and a daemon stopped first leaves its
    /// containers running -- by design, so that restarting it never
    /// interrupts them.
    async fn turn_off(&self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        if is_active(&self.control.active_state(SERVICE_UNIT).await?) {
            self.ready().await?;
            let listed = self
                .daemon
                .containers()
                .await
                .context("list mica-containerd's containers")?;
            removed = declared_names(&listed);
            self.daemon
                .declare(&BTreeMap::new())
                .await
                .context("declare no container to mica-containerd")?;
            self.drain(&removed).await?;
            self.control.stop(SERVICE_UNIT).await?;
        }
        if is_enabled(&self.control.unit_file_state(SERVICE_UNIT).await?) {
            self.control.disable(SERVICE_UNIT).await?;
        }
        Ok(removed)
    }

    /// Wait until the engine lists none of `names`.
    async fn drain(&self, names: &[String]) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + self.drain_timeout;
        loop {
            let listed = self.engine.containers().await;
            if listed["available"] != serde_json::json!(true) {
                bail!(
                    "the container engine cannot say whether {names:?} are gone, so mica-containerd is left running: {}",
                    listed["detail"]
                );
            }
            let present = engine_names(&listed);
            let left: Vec<&String> = names.iter().filter(|n| present.contains(*n)).collect();
            if left.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "mica-containerd did not remove {left:?} within {} seconds; it is left running so they are not orphaned",
                    self.drain_timeout.as_secs()
                );
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// The names of the daemon's declarations, from `GET /v1/containers`.
fn declared_names(listed: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = listed["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["spec"]["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Every container name `podman ps --all` lists.
fn engine_names(listed: &serde_json::Value) -> Vec<String> {
    listed["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| entry["Names"].as_array().cloned().unwrap_or_default())
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect()
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for ContainerReconciler<C> {
    fn name(&self) -> &'static str {
        "container"
    }

    fn subtree(&self) -> &'static str {
        "container"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let enabled = settings.container.enabled;
        let declared = &settings.container.units;
        let (containers, removed) = if enabled {
            (Some(self.turn_on(declared).await?), Vec::new())
        } else {
            (None, self.turn_off().await?)
        };
        let leftovers = self.remove_leftovers()?;
        // Read after the transition: what is published is what the system now
        // is, not what it was asked to become.
        let service_state = self.control.active_state(SERVICE_UNIT).await?;
        Ok(serde_json::json!({
            "enabled": enabled,
            "service": SERVICE_UNIT,
            "serviceState": service_state,
            "declaredCount": declared.len(),
            // The daemon's own answer to the declaration: every container with
            // its phase. Absent with the switch off.
            "containers": containers.map(|answer| answer["containers"].clone()),
            "removedContainers": removed,
            "removedQuadletFiles": leftovers,
        }))
    }
}

#[cfg(test)]
mod tests;
