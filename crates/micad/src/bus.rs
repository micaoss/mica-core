//! D-Bus service implementation for `com.mica.micad`.
//!
//! Exposes the settings tree and the live-state tree on the bus as the
//! `com.mica.micad1` interface at [`OBJECT_PATH`], owned under [`BUS_NAME`].

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use micad_settings::{DocumentRefusal, Settings, Store};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use zbus::fdo;

use crate::apply_queue::ApplyQueue;
use crate::bluetooth::{Agent as BluetoothAgent, BluetoothControl};
use crate::containers::ContainerEngine;
use crate::deployment::{DeploymentClient, NativeClient};
use crate::diagnostics::{FailureEvidenceSource, UnavailableFailureEvidence};
use crate::network_state::{NetworkState, UnavailableNetworkState};
use crate::power::PowerControl;
use crate::reconciler::Reconciler;
use crate::reconciler::network::WireguardRotate;
use crate::scan::Registry;
use crate::storage_status::{PressureTracker, StorageStatusSource, UnavailableStorageStatus};
use crate::system_info::{SystemInfoSource, UnavailableSystemInfo};
use crate::telemetry::{TelemetrySource, UnavailableTelemetry};
use crate::time_status::{TimeStatusSource, UnavailableTimeStatus};
use crate::update_lifecycle::{DEFAULT_WORKSPACE_ROOT, LifecycleHost, NoClient, UpdateLifecycle};
use crate::update_policy::PolicyStore;

mod apply;
mod builder;
mod errors;
mod interface;
mod operations;
mod power;
mod served;

pub use errors::*;
pub use served::*;

/// Well-known bus name owned by the daemon.
pub const BUS_NAME: &str = "com.mica.micad";
/// Object path the service is registered at.
pub const OBJECT_PATH: &str = "/com/mica/micad";

/// True when `a` and `b` overlap by dot segments in either direction:
/// one path is a segment-wise prefix of the other. The root path (`""` or
/// `"."`) matches everything.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let a = if a == "." { "" } else { a };
    let b = if b == "." { "" } else { b };
    if a.is_empty() || b.is_empty() {
        return true;
    }
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => {}
            (Some(_), Some(_)) => return false,
            _ => return true,
        }
    }
}

/// Mutable trees guarded by one lock so settings writes and live-state
/// updates stay consistent.
struct Inner {
    settings: Settings,
    state: Value,
    /// The network map the network reconciler last applied without error:
    /// what a network apply that did not come up puts back.
    applied_network: Option<std::collections::BTreeMap<String, micad_settings::IfaceSettings>>,
}

/// The `com.mica.micad1` service: settings tree, live-state tree, store,
/// reconcilers, the power control, the update installer client and the shadow
/// file a transient root password is written into.
/// The most lines one `GetContainerLog` answers.
const MAX_CONTAINER_LOG_LINES: u32 = 1000;
/// The most bytes one `GetContainerLog` answers: the newest are kept.
const MAX_CONTAINER_LOG_BYTES: usize = 256 * 1024;

/// Whether `name` can be a Compose service's: 1 to 20 of `[a-z0-9_-]`.
fn is_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 20
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Whether `name` can be a container's, a compose project's or a service's on
/// dockerd: 1 to 128 of `[A-Za-z0-9_.-]`, starting with a letter or a digit.
fn docker_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The three verbs a declared container takes.
#[derive(Debug, Clone, Copy)]
enum ContainerAction {
    Start,
    Stop,
    Restart,
}

pub struct MicadService {
    store: Store,
    reconcilers: Arc<Vec<Box<dyn Reconciler>>>,
    power: Box<dyn PowerControl>,
    deployments: Arc<dyn DeploymentClient>,
    /// Shared interlock for install admission and reboot refusal.
    installing: Arc<AtomicBool>,
    shadow_path: PathBuf,
    /// `Arc` so the install background task can record its outcome into the
    /// live-state tree after the bus call that spawned it has returned.
    inner: Arc<RwLock<Inner>>,
    /// Serializes every settings reconcile and the secret/key mutations whose
    /// read-modify-write cycles must not interleave. Data readers never take
    /// this lock.
    apply_lock: Arc<Mutex<()>>,
    /// Work queue and bounded lifecycle history for settings applies.
    apply_queue: Arc<ApplyQueue>,
    /// The service registry the scan task fills ([`crate::scan`]), shared so
    /// that `ForgetService` drops an entry from the same table the scan
    /// publishes from — one table, so the bus surface and the live-state tree
    /// cannot disagree about which services exist.
    registry: Option<Arc<Registry>>,
    /// The WireGuard key rotation `RotateWireguardKey` calls through.
    ///
    /// Defaults to [`NoRotation`], which draws no key at all: a daemon that
    /// was never handed a rotation is one running against no STATE partition,
    /// and the honest answer there is that there is nowhere to put a key.
    wireguard: Arc<dyn WireguardRotate>,
    /// Read-only live network observation. The default is unavailable so
    /// tests and dry-run instances never inspect the host network.
    network_state: Arc<dyn NetworkState>,
    /// The resolution check `GetDnsProbe` reads and `ProbeDns` runs; none in
    /// tests and dry-run instances, for the same reason.
    dns_probe: Option<Arc<crate::network_state::DnsProbe>>,
    /// Read-only time-synchronization observation, on the same default for
    /// the same reason.
    time_status: Arc<dyn TimeStatusSource>,
    /// Read-only storage observation, on the same default again.
    storage_status: Arc<dyn StorageStatusSource>,
    /// The low-space hysteresis state the storage surface classifies against.
    /// Lives with the service rather than with the observer because it is the
    /// REPORTED state, and it has to survive an observer being reattached.
    storage_pressure: Arc<PressureTracker>,
    /// Read-only system-information observation, on the same
    /// unavailable default for the same reason.
    system_info: Arc<dyn SystemInfoSource>,
    /// Read-only board telemetry, on the same default.
    telemetry: Arc<dyn TelemetrySource>,
    /// Read-only container observation: what the engine says is there.
    ///
    /// Read-only in the strong sense -- the adapter runs `podman images` and
    /// nothing that writes. Containers are declared as settings and their
    /// lifecycle belongs to mica-containerd. Same unavailable default as every
    /// other observer.
    engine: Arc<dyn ContainerEngine>,
    /// The Bluetooth adapter, its devices and its pairing verbs.
    ///
    /// The default has no adapter, so a board with no radio, a dry run and a
    /// test all answer "this device has no Bluetooth adapter" rather than a
    /// list of devices nobody has.
    bluetooth: Arc<dyn BluetoothControl>,
    /// The pairing agent's shared state: what is waiting for a confirmation,
    /// and the code a legacy peer is answered with.
    ///
    /// Shared with the `org.bluez.Agent1` object BlueZ calls, which is the
    /// whole reason it is here: the agent blocks on a decision that arrives
    /// through the bus members below.
    agent: Arc<BluetoothAgent>,
    /// The features the product carries. A member or a settings write that
    /// belongs to any other is refused by name.
    features: micad_settings::Features,
    /// mica-containerd, which the three container actions and the containers
    /// read go to.
    ///
    /// `StartContainer` and its two siblings are the daemon's verbs, never
    /// podman's: the daemon persists them as the container's desired state,
    /// and a container podman started behind its back is one it removes.
    containerd: Arc<dyn crate::containerd::Containerd>,
    /// The engine the product carries, which `GetContainers` names.
    container_engine: micad_settings::ContainerEngine,
    /// The check a project's Compose text is held to when it is written, and
    /// for `CheckContainerProject`.
    compose: Arc<dyn crate::compose::ComposeCheck>,
    /// Read-only failure evidence for the diagnostic snapshot:
    /// failed units and a bounded journal excerpt. Same default.
    failure_evidence: Arc<dyn FailureEvidenceSource>,
    /// One service's log for the console (`GetLog`). Same default.
    logs: Arc<dyn crate::logs::LogReader>,
    /// The user's own services: `GetUnits`, the unit verbs and logs, and the
    /// check a write of `services.units` is held to. The default has no
    /// `mica-unit`, so nothing is installed and every write is refused.
    user_units: Arc<crate::user_units::UserUnits>,
    /// The update lifecycle (check/fetch machine, policy, reboot gate).
    /// Constructed with [`NoClient`] and a fileless policy store, so a
    /// dry-run daemon can neither spawn the update client nor read a host
    /// policy file; production attaches both via [`Self::with_update`].
    update: Arc<UpdateLifecycle>,
    /// The `/mica/config/` documents this boot found and refused (**the pour**).
    refusals: Arc<RwLock<Vec<DocumentRefusal>>>,
}

/// The lifecycle's window onto this service: it records under
/// `update.lifecycle` and reads the `health` subtree, and nothing else.
struct InnerLifecycleHost(Arc<RwLock<Inner>>);

#[async_trait::async_trait]
impl LifecycleHost for InnerLifecycleHost {
    async fn record(&self, lifecycle: Value) {
        let mut inner = self.0.write().await;
        update_entry(&mut inner.state).insert("lifecycle".into(), lifecycle);
        drop(inner);
    }

    async fn health(&self) -> Value {
        let inner = self.0.read().await;
        inner
            .state
            .get("health")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    }
}

fn update_entry(state: &mut Value) -> &mut serde_json::Map<String, Value> {
    let update = object_of(state)
        .entry("update")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    object_of(update)
}

/// `value` as an object, made one first if it is not: the live-state root and
/// its `update` entry are objects by construction, and a write never panics.
fn object_of(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
    match value {
        Value::Object(map) => map,
        _ => unreachable!("just made an object"),
    }
}

/// The rotation a daemon with no key store has: none.
///
/// The dry-run shape [`NoClient`] and [`crate::power::DryRunPower`]
/// both take — a default that cannot touch the host, so only `main.rs`, which
/// alone knows the daemon is running on a device, can attach one that can.
struct NoRotation;

#[async_trait::async_trait]
impl WireguardRotate for NoRotation {
    async fn rotate_key(&self, _iface: &str) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("this daemon has no WireGuard key store"))
    }
}

impl MicadService {
    /// Build the service around loaded `settings` and an initial live-state
    /// root (an empty object, or `{"dry_run": true}` in dry-run mode).
    ///
    /// `shadow_path` is a parameter for the same reason every reconciler path
    /// is: a test points it at a temporary file and can then drive the real
    /// transient-password method without touching the host's `/etc/shadow`.
    pub fn new(
        store: Store,
        settings: Settings,
        reconcilers: Vec<Box<dyn Reconciler>>,
        power: Box<dyn PowerControl>,
        shadow_path: PathBuf,
        state: Value,
    ) -> Self {
        let installing = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(RwLock::new(Inner {
            settings,
            state,
            applied_network: None,
        }));
        let update = Arc::new(UpdateLifecycle::new(
            Arc::new(NoClient),
            PolicyStore::defaults(),
            Arc::new(InnerLifecycleHost(Arc::clone(&inner))),
            Arc::clone(&installing),
            PathBuf::from(DEFAULT_WORKSPACE_ROOT),
        ));
        Self {
            store,
            reconcilers: Arc::new(reconcilers),
            power,
            deployments: Arc::new(NativeClient::new(Arc::new(NoClient))),
            installing,
            shadow_path,
            inner,
            apply_lock: Arc::new(Mutex::new(())),
            apply_queue: Arc::new(ApplyQueue::new()),
            registry: None,
            wireguard: Arc::new(NoRotation),
            network_state: Arc::new(UnavailableNetworkState),
            dns_probe: None,
            time_status: Arc::new(UnavailableTimeStatus),
            storage_status: Arc::new(UnavailableStorageStatus),
            system_info: Arc::new(UnavailableSystemInfo),
            telemetry: Arc::new(UnavailableTelemetry),
            failure_evidence: Arc::new(UnavailableFailureEvidence),
            logs: Arc::new(crate::logs::NoLogs),
            user_units: Arc::new(crate::user_units::UserUnits::unavailable()),
            engine: Arc::new(crate::containers::NoEngine),
            bluetooth: Arc::new(crate::bluetooth::NoAdapter),
            agent: Arc::new(BluetoothAgent::default()),
            containerd: Arc::new(crate::containerd::NoContainerd),
            container_engine: micad_settings::ContainerEngine::default(),
            compose: Arc::new(crate::compose::NoCompose),
            features: micad_settings::Features::all(),
            storage_pressure: Arc::new(PressureTracker::default()),
            update,
            refusals: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// The pairing code this device answers a legacy peer with.
    ///
    /// Read here rather than stored, so the console and the agent cannot
    /// disagree about what a peer will be told.
    fn pairing_pin(&self, settings: &micad_settings::Settings) -> String {
        settings.bluetooth.pairing_pin().to_string()
    }

    /// Refuse a Bluetooth action on a device that has no adapter or has it
    /// switched off, rather than letting it fail as a bus error about an
    /// object that does not exist.
    async fn require_bluetooth(&self) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        if !self.inner.read().await.settings.bluetooth.enabled {
            return Err(fdo::Error::Failed(
                "Bluetooth is switched off on this device; enable `bluetooth.enabled` first"
                    .to_string(),
            ));
        }
        self.bluetooth
            .adapter()
            .await
            .map(|_| ())
            .map_err(|err| fdo::Error::Failed(format!("{err:#}")))
    }

    /// What mica-containerd says of its projects, in the envelope every
    /// observer uses: an absent daemon is unavailable, never an empty list.
    async fn daemon_containers(&self) -> Value {
        match self.containerd.projects().await {
            Ok(answer) => serde_json::json!({
                "available": true,
                "projects": answer["projects"],
            }),
            Err(err) => serde_json::json!({ "available": false, "detail": err.to_string() }),
        }
    }

    /// The containers `name` stands for: `<project>/<service>`'s one, or
    /// every container of `<project>`.
    ///
    /// The name is checked against the DECLARED settings first, so this is not
    /// a way to drive an arbitrary container through a declared-container
    /// argument: a name nobody declared is refused before any call. On docker
    /// nothing is declared and every container is dockerd's: a container
    /// compose did not make is the project and the service of its own name.
    async fn declared_containers(
        &self,
        name: &str,
        whole_project: bool,
    ) -> fdo::Result<Vec<String>> {
        self.require_feature(micad_settings::Feature::Containers)?;
        let settings = self.inner.read().await.settings.clone();
        let container = &settings.container;
        let undeclared = || {
            fdo::Error::InvalidArgs(format!(
                "no project or service named {name:?} is declared on this device"
            ))
        };
        // On docker every container dockerd has is named, by its compose
        // project and service or by its own name, and nothing is declared.
        let docker = self.container_engine == micad_settings::ContainerEngine::Docker;
        let declared = |project: &str| {
            if docker {
                docker_name(project)
            } else {
                container.projects.contains_key(project)
            }
        };
        let (project, service) = match name.split_once('/') {
            Some((project, service))
                if declared(project)
                    && (is_service_name(service) || (docker && docker_name(service))) =>
            {
                (project, Some(service))
            }
            None if whole_project && declared(name) => (name, None),
            _ => return Err(undeclared()),
        };
        if !container.enabled {
            return Err(fdo::Error::Failed(
                "containers are switched off on this device; enable `container.enabled` first"
                    .to_string(),
            ));
        }
        // A service's container is the one mica-containerd names for it: its
        // `container_name`, else `<project>-<service>-1`.
        let answer = self
            .containerd
            .projects()
            .await
            .map_err(|err| fdo::Error::Failed(format!("list the projects: {err}")))?;
        let names: Vec<String> = answer["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["name"] == serde_json::json!(project))
            .flat_map(|entry| entry["services"].as_array().cloned().unwrap_or_default())
            .filter(|entry| {
                service.is_none_or(|service| entry["service"] == serde_json::json!(service))
            })
            .filter_map(|entry| entry["spec"]["name"].as_str().map(str::to_string))
            .collect();
        if names.is_empty() {
            return Err(fdo::Error::Failed(format!(
                "the container engine holds no container of {name:?} yet"
            )));
        }
        Ok(names)
    }

    /// One container lifecycle verb, mica-containerd's, on what `name` stands
    /// for ([`Self::declared_containers`]).
    async fn container_action(&self, name: &str, action: ContainerAction) -> fdo::Result<()> {
        let verb = match action {
            ContainerAction::Start => crate::containerd::Verb::Start,
            ContainerAction::Stop => crate::containerd::Verb::Stop,
            ContainerAction::Restart => crate::containerd::Verb::Restart,
        };
        for container in self.declared_containers(name, true).await? {
            self.containerd
                .act(&container, verb)
                .await
                .map_err(|err| fdo::Error::Failed(format!("{action:?} {container}: {err}")))?;
            tracing::info!(container, ?action, "container action");
        }
        Ok(())
    }

    /// One verb on an installed user service.
    async fn unit_action(&self, name: &str, verb: crate::user_units::Verb) -> fdo::Result<()> {
        self.user_units
            .require_installed(name)
            .await
            .map_err(|err| fdo::Error::InvalidArgs(format!("{err:#}")))?;
        self.user_units
            .act(name, verb)
            .await
            .map_err(|err| fdo::Error::Failed(format!("{verb:?} {name}: {err:#}")))?;
        tracing::info!(unit = name, ?verb, "unit action");
        Ok(())
    }

    /// The last `tail` lines of one declared container's log, bounded.
    async fn container_log(&self, name: &str, tail: u32) -> fdo::Result<String> {
        if !(1..=MAX_CONTAINER_LOG_LINES).contains(&tail) {
            return Err(fdo::Error::InvalidArgs(format!(
                "a log is 1 to {MAX_CONTAINER_LOG_LINES} lines, not {tail}"
            )));
        }
        let containers = self.declared_containers(name, false).await?;
        let mut log = self
            .containerd
            .logs(&containers[0], tail)
            .await
            .map_err(|err| fdo::Error::Failed(format!("the log of {name}: {err}")))?;
        if log.len() > MAX_CONTAINER_LOG_BYTES {
            let mut cut = log.len() - MAX_CONTAINER_LOG_BYTES;
            while !log.is_char_boundary(cut) {
                cut += 1;
            }
            log.drain(..cut);
        }
        Ok(log)
    }

    /// Check one project's Compose text, as a write would: in `dir`, else in
    /// the project's stored directory.
    async fn check_container_project_now(
        &self,
        project: &str,
        text: &str,
        dir: Option<&str>,
    ) -> fdo::Result<Value> {
        self.require_feature(micad_settings::Feature::Containers)?;
        if self.container_engine == micad_settings::ContainerEngine::Docker {
            return Err(fdo::Error::NotSupported(
                crate::docker::USE_DOCKER_COMPOSE.into(),
            ));
        }
        let mut one = micad_settings::ContainerSettings {
            projects: std::collections::BTreeMap::from([(project.to_string(), text.to_string())]),
            ..Default::default()
        };
        micad_settings::validate_container_projects(&one.projects)
            .map_err(fdo::Error::InvalidArgs)?;
        let dir = match dir {
            Some(dir) => Some(dir.to_string()),
            None => self
                .inner
                .read()
                .await
                .settings
                .container
                .project_dirs
                .get(project)
                .cloned(),
        };
        if let Some(dir) = &dir {
            one.project_dirs.insert(project.to_string(), dir.clone());
            micad_settings::validate_container_project_dirs(&one)
                .map_err(fdo::Error::InvalidArgs)?;
        }
        self.compose
            .check(project, text, dir.as_deref())
            .await
            .map_err(fdo::Error::Failed)
    }

    /// Record the `/mica/config/` documents this boot refused, and publish them
    /// into the live-state tree.
    pub async fn set_config_refusals(&self, refusals: Vec<DocumentRefusal>) {
        *self.refusals.write().await = refusals;
        self.publish_refusals().await;
    }

    /// Mirror the current refusals into `configuration.refused` in the
    /// live-state tree.
    async fn publish_refusals(&self) {
        let refused: Vec<Value> = self
            .refusals
            .read()
            .await
            .iter()
            .map(|refusal| {
                serde_json::json!({
                    "document": refusal.document,
                    "path": refusal.path.display().to_string(),
                    "message": refusal.message,
                    "subtrees": refusal.subtrees,
                })
            })
            .collect();
        let mut inner = self.inner.write().await;
        if let Some(root) = inner.state.as_object_mut() {
            root.insert(
                "configuration".to_string(),
                serde_json::json!({ "refused": refused }),
            );
        }
    }
}

#[cfg(test)]
mod tests;
