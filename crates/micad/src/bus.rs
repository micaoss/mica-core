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
}

/// The `com.mica.micad1` service: settings tree, live-state tree, store,
/// reconcilers, the power control, the update installer client and the shadow
/// file a transient root password is written into.
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
    /// Read-only failure evidence for the diagnostic snapshot:
    /// failed units and a bounded journal excerpt. Same default.
    failure_evidence: Arc<dyn FailureEvidenceSource>,
    /// One service's log for the console (`GetLog`). Same default.
    logs: Arc<dyn crate::logs::LogReader>,
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
        let inner = Arc::new(RwLock::new(Inner { settings, state }));
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
            time_status: Arc::new(UnavailableTimeStatus),
            storage_status: Arc::new(UnavailableStorageStatus),
            system_info: Arc::new(UnavailableSystemInfo),
            telemetry: Arc::new(UnavailableTelemetry),
            failure_evidence: Arc::new(UnavailableFailureEvidence),
            logs: Arc::new(crate::logs::NoLogs),
            engine: Arc::new(crate::containers::NoEngine),
            bluetooth: Arc::new(crate::bluetooth::NoAdapter),
            agent: Arc::new(BluetoothAgent::default()),
            containerd: Arc::new(crate::containerd::NoContainerd),
            features: micad_settings::Features::all(),
            storage_pressure: Arc::new(PressureTracker::default()),
            update,
            refusals: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// The pairing code this device answers a legacy peer with.
    ///
    /// The declared one, or the value derived from the device identity. Read
    /// here rather than stored, so the console and the agent cannot disagree
    /// about what a peer will be told.
    fn pairing_pin(&self, settings: &micad_settings::Settings) -> String {
        match &settings.bluetooth.pin {
            Some(pin) => pin.clone(),
            None => micad_settings::derived_pairing_pin(
                settings
                    .provisioning
                    .device_id
                    .as_deref()
                    .unwrap_or_default(),
            ),
        }
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

    /// What mica-containerd says of its containers, in the envelope every
    /// observer uses: an absent daemon is unavailable, never an empty list.
    async fn daemon_containers(&self) -> Value {
        match self.containerd.containers().await {
            Ok(answer) => serde_json::json!({
                "available": true,
                "entries": answer["containers"],
            }),
            Err(err) => serde_json::json!({ "available": false, "detail": err.to_string() }),
        }
    }

    /// One container lifecycle verb, mica-containerd's.
    ///
    /// The name is checked against the DECLARED map first, so this is not a
    /// way to drive an arbitrary container through a declared-container
    /// argument: a name nobody declared is refused before any call.
    async fn container_action(&self, name: &str, action: ContainerAction) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Containers)?;
        let settings = self.inner.read().await.settings.clone();
        if !settings.container.units.contains_key(name) {
            return Err(fdo::Error::InvalidArgs(format!(
                "no container named {name:?} is declared on this device"
            )));
        }
        if !settings.container.enabled {
            return Err(fdo::Error::Failed(
                "containers are switched off on this device; enable `container.enabled` first"
                    .to_string(),
            ));
        }
        let verb = match action {
            ContainerAction::Start => crate::containerd::Verb::Start,
            ContainerAction::Stop => crate::containerd::Verb::Stop,
            ContainerAction::Restart => crate::containerd::Verb::Restart,
        };
        self.containerd
            .act(name, verb)
            .await
            .map_err(|err| fdo::Error::Failed(format!("{action:?} {name}: {err}")))?;
        tracing::info!(container = name, ?action, "container action");
        Ok(())
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
