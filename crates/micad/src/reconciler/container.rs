//! Reconciler for the `container` settings subtree, on either init and either
//! engine.
//!
//! On podman, mica-podman's supervisor, mica-containerd, owns every container
//! (`crate::containerd`). This reconciler starts it -- `mica-containerd.service`
//! or the OpenRC script of the same name, through [`UnitControl`] -- and
//! declares the settings' Compose projects to it as one set. The daemon
//! compares, recreates only what changed and brings them back at boot on its
//! own; micad runs no container.
//!
//! On docker it only switches `docker.service` on and off, the same way and
//! with nothing declared: dockerd starts its containers by their restart
//! policies, and stopping it stops them (`crate::docker`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use micad_settings::{ContainerSettings, Settings};

use super::Reconciler;
use super::systemd::{UnitControl, is_active, is_enabled};
use crate::containerd::{Containerd, SERVICE_UNIT, Verb};
use crate::containers::ContainerEngine;

/// How long the daemon may take to answer after its service started.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long dockerd may take: a cold start on eMMC is slower.
const DOCKER_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the containers may take to stop once each is told to: the daemon
/// stops them on its next pass, not in the call.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);
/// Between two looks while waiting.
const POLL: Duration = Duration::from_millis(500);

/// Reconciler for the `container` subtree.
pub struct ContainerReconciler<C: UnitControl> {
    control: C,
    /// The engine's service: mica-containerd's, or dockerd's.
    service: &'static str,
    /// Whether the projects are declared to the engine and its containers
    /// driven through a turn; dockerd's are its own.
    declares: bool,
    daemon: Arc<dyn Containerd>,
    engine: Arc<dyn ContainerEngine>,
    ready_timeout: Duration,
    drain_timeout: Duration,
    poll: Duration,
}

impl<C: UnitControl> ContainerReconciler<C> {
    /// A reconciler driving `daemon` and its service through `control`, and
    /// reading `engine` to see containers stop.
    pub fn new(control: C, daemon: Arc<dyn Containerd>, engine: Arc<dyn ContainerEngine>) -> Self {
        Self {
            control,
            service: SERVICE_UNIT,
            declares: true,
            daemon,
            engine,
            ready_timeout: READY_TIMEOUT,
            drain_timeout: DRAIN_TIMEOUT,
            poll: POLL,
        }
    }

    /// The installed engine the product names: mica-containerd and podman,
    /// or dockerd.
    pub fn production(control: C, engine: micad_settings::ContainerEngine) -> Self {
        match engine {
            micad_settings::ContainerEngine::Podman => Self::new(
                control,
                Arc::new(crate::containerd::Client::production()),
                Arc::new(crate::containers::Podman::default()),
            ),
            micad_settings::ContainerEngine::Docker => {
                let docker = Arc::new(crate::docker::Docker::production());
                Self::new(control, Arc::clone(&docker) as Arc<dyn Containerd>, docker)
                    .service_only(crate::docker::SERVICE_UNIT, DOCKER_READY_TIMEOUT)
            }
        }
    }

    /// Only switch `service` on and off, waiting up to `ready` for it to
    /// answer: the engine it runs starts and stops its own containers.
    #[must_use]
    pub fn service_only(mut self, service: &'static str, ready: Duration) -> Self {
        self.service = service;
        self.ready_timeout = ready;
        self.declares = false;
        self
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
                        "{} did not answer within {} seconds of starting: {err}",
                        self.service,
                        self.ready_timeout.as_secs()
                    )
                }
                Err(_) => tokio::time::sleep(self.poll).await,
            }
        }
    }

    /// Enable and start the service; whether this pass started it.
    async fn run_service(&self) -> Result<bool> {
        if !is_enabled(&self.control.unit_file_state(self.service).await?) {
            self.control.enable(self.service).await?;
        }
        let started = !is_active(&self.control.active_state(self.service).await?);
        if started {
            self.control.start(self.service).await?;
        }
        self.ready().await?;
        Ok(started)
    }

    /// Declare the projects as one set. One call per pass, idempotent: the
    /// daemon compares and recreates only what changed, and writes nothing
    /// when nothing did.
    async fn turn_on(&self, settings: &ContainerSettings) -> Result<Declared> {
        let started = self.run_service().await?;
        if !self.declares {
            return Ok(Declared::default());
        }
        let answer = self
            .daemon
            .declare_projects(settings)
            .await
            .context("declare the projects")?;
        // A switch turned off stopped every container, and a stop holds until
        // a start: the pass that brings the service back starts them.
        let mut started_containers = Vec::new();
        if started {
            for name in project_containers(&answer) {
                self.daemon
                    .act(&name, Verb::Start)
                    .await
                    .with_context(|| format!("start {name}"))?;
                started_containers.push(name);
            }
        }
        Ok(Declared {
            projects: answer["projects"].clone(),
            started: started_containers,
        })
    }

    /// Stop every container, wait until none runs, then stop the daemon.
    ///
    /// The order is the point: a daemon stopped first leaves its containers
    /// running -- by design, so that restarting it never interrupts them. The
    /// projects stay declared. dockerd is only stopped: it stops its
    /// containers itself, as `systemctl stop docker` has it do.
    async fn turn_off(&self) -> Result<Vec<String>> {
        let mut stopped = Vec::new();
        if !self.declares {
            if is_active(&self.control.active_state(self.service).await?) {
                self.control.stop(self.service).await?;
            }
        } else if is_active(&self.control.active_state(self.service).await?) {
            self.ready().await?;
            let answer = self.daemon.projects().await.context("list the projects")?;
            stopped = project_containers(&answer);
            for name in &stopped {
                self.daemon
                    .act(name, Verb::Stop)
                    .await
                    .with_context(|| format!("stop {name}"))?;
            }
            self.drain(&stopped).await?;
            self.control.stop(self.service).await?;
        }
        if is_enabled(&self.control.unit_file_state(self.service).await?) {
            self.control.disable(self.service).await?;
        }
        Ok(stopped)
    }

    /// Wait until the engine runs none of `stopped`.
    async fn drain(&self, stopped: &[String]) -> Result<()> {
        if stopped.is_empty() {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + self.drain_timeout;
        loop {
            let listed = self.engine.containers().await;
            if listed["available"] != serde_json::json!(true) {
                bail!(
                    "the container engine cannot say whether {stopped:?} stopped, so {} is left running: {}",
                    self.service,
                    listed["detail"]
                );
            }
            let running = running_names(&listed);
            let up: Vec<&String> = stopped.iter().filter(|n| running.contains(*n)).collect();
            if up.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "{} did not stop {up:?} within {} seconds; it is left running so they are not orphaned",
                    self.service,
                    self.drain_timeout.as_secs()
                );
            }
            tokio::time::sleep(self.poll).await;
        }
    }
}

/// What one pass declared, as the daemon answered it.
#[derive(Default)]
struct Declared {
    /// The answer to `PUT /v1/projects`.
    projects: serde_json::Value,
    /// The containers this pass started.
    started: Vec<String>,
}

/// The containers of every project, from `GET` or `PUT /v1/projects`.
pub(crate) fn project_containers(answer: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = answer["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|project| project["services"].as_array().cloned().unwrap_or_default())
        .filter_map(|entry| entry["spec"]["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Every running container's name, as `podman ps --all` lists it.
fn running_names(listed: &serde_json::Value) -> Vec<String> {
    listed["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["State"] == serde_json::json!("running"))
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
        let container = &settings.container;
        let enabled = container.enabled;
        let (declared, stopped) = if enabled {
            (self.turn_on(container).await?, Vec::new())
        } else {
            (Declared::default(), self.turn_off().await?)
        };
        // Read after the transition: what is published is what the system now
        // is, not what it was asked to become.
        let service_state = self.control.active_state(self.service).await?;
        Ok(serde_json::json!({
            "enabled": enabled,
            "service": self.service,
            "serviceState": service_state,
            "declaredProjects": container.projects.keys().collect::<Vec<_>>(),
            // The daemon's own answer to the declaration: every project with
            // its containers' phases. Null with the switch off.
            "projects": declared.projects,
            "startedContainers": declared.started,
            "stoppedContainers": stopped,
        }))
    }
}

#[cfg(test)]
mod tests;
