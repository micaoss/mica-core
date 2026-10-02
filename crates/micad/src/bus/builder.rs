//! Building the service: the sources and clients it is given.

use crate::bluetooth::{Agent as BluetoothAgent, BluetoothControl};
use crate::containers::ContainerEngine;
use crate::deployment::NativeClient;
use crate::diagnostics::FailureEvidenceSource;
use crate::network_state::NetworkState;
use crate::reconciler::network::WireguardRotate;
use crate::scan::Registry;
use crate::storage_status::StorageStatusSource;
use crate::system_info::SystemInfoSource;
use crate::telemetry::TelemetrySource;
use crate::time_status::TimeStatusSource;
use crate::update_lifecycle::{UpdateClient, UpdateLifecycle};
use crate::update_policy::PolicyStore;
use serde_json::Value;
use std::sync::Arc;
use zbus::fdo;

use super::*;

impl MicadService {
    /// Attach the native command transport and operator policy.
    #[must_use]
    pub fn with_update(mut self, client: Arc<dyn UpdateClient>, policy: PolicyStore) -> Self {
        self.deployments = Arc::new(NativeClient::new(Arc::clone(&client)));
        self.update = Arc::new(UpdateLifecycle::new(
            client,
            policy,
            Arc::new(InnerLifecycleHost(Arc::clone(&self.inner))),
            Arc::clone(&self.installing),
            self.update.workspace_root().to_path_buf(),
        ));
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_update_workspace(mut self, root: PathBuf) -> Self {
        self.update = Arc::new(UpdateLifecycle::new(
            self.update.client(),
            self.update.policy(),
            Arc::new(InnerLifecycleHost(Arc::clone(&self.inner))),
            Arc::clone(&self.installing),
            root,
        ));
        self
    }

    /// The lifecycle handle `main.rs` gives the auto-check task.
    pub fn update_handle(&self) -> Arc<UpdateLifecycle> {
        Arc::clone(&self.update)
    }

    /// Attach the WireGuard key rotation.
    ///
    /// A builder step for the same reason [`Self::with_update`] is: the default
    /// touches nothing, and a test that has no state directory must not be
    /// able to draw a key into one.
    #[must_use]
    pub fn with_wireguard(mut self, wireguard: Arc<dyn WireguardRotate>) -> Self {
        self.wireguard = wireguard;
        self
    }

    /// Attach the production network observer.
    #[must_use]
    pub fn with_network_state(mut self, network_state: Arc<dyn NetworkState>) -> Self {
        self.network_state = network_state;
        self
    }

    /// Attach the production time-synchronization observer.
    #[must_use]
    pub fn with_time_status(mut self, time_status: Arc<dyn TimeStatusSource>) -> Self {
        self.time_status = time_status;
        self
    }

    /// Attach the production storage observer.
    ///
    /// The default observes nothing, so a dry-run daemon or a test never
    /// reads the host's sysfs, mount table or media — and never refuses an
    /// install over free space it cannot see.
    #[must_use]
    pub fn with_storage_status(mut self, storage_status: Arc<dyn StorageStatusSource>) -> Self {
        self.storage_status = storage_status;
        self
    }

    /// Attach the system-information observer (`GetSystemInfo`).
    ///
    /// Same shape and same reason as [`Self::with_storage_status`]: only
    /// `main.rs` knows the daemon runs on a device.
    #[must_use]
    pub fn with_system_info(mut self, system_info: Arc<dyn SystemInfoSource>) -> Self {
        self.system_info = system_info;
        self
    }

    /// Attach the container engine's read and mica-containerd, which the
    /// container actions and the containers read go to (`GetContainers`,
    /// `StartContainer` and its siblings).
    #[must_use]
    pub fn with_containers(
        mut self,
        engine: Arc<dyn ContainerEngine>,
        containerd: Arc<dyn crate::containerd::Containerd>,
    ) -> Self {
        self.engine = engine;
        self.containerd = containerd;
        self
    }

    /// Serve only `features`.
    #[must_use]
    pub fn with_features(mut self, features: micad_settings::Features) -> Self {
        self.features = features;
        self
    }

    /// Refuse a member of a feature the product does not carry, by name,
    /// rather than letting it fail against hardware or a daemon that is not
    /// there.
    pub(super) fn require_feature(&self, feature: micad_settings::Feature) -> fdo::Result<()> {
        if self.features.has(feature) {
            Ok(())
        } else {
            Err(fdo::Error::NotSupported(format!(
                "feature not in this product: {feature}"
            )))
        }
    }

    /// Attach the Bluetooth adapter and the pairing agent's state
    /// (`GetBluetooth` and the pairing actions).
    #[must_use]
    pub fn with_bluetooth(
        mut self,
        bluetooth: Arc<dyn BluetoothControl>,
        agent: Arc<BluetoothAgent>,
    ) -> Self {
        self.bluetooth = bluetooth;
        self.agent = agent;
        self
    }

    /// Attach the board telemetry adapter (`GetTelemetry`).
    #[must_use]
    pub fn with_telemetry(mut self, telemetry: Arc<dyn TelemetrySource>) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Attach the log reader (`GetLog`).
    #[must_use]
    pub fn with_logs(mut self, logs: Arc<dyn crate::logs::LogReader>) -> Self {
        self.logs = logs;
        self
    }

    /// Attach the failure-evidence source (`GetFailureEvidence`).
    #[must_use]
    pub fn with_failure_evidence(
        mut self,
        failure_evidence: Arc<dyn FailureEvidenceSource>,
    ) -> Self {
        self.failure_evidence = failure_evidence;
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_deployments(mut self, deployments: Arc<dyn DeploymentClient>) -> Self {
        self.deployments = deployments;
        self
    }

    /// Attach the service registry this daemon's scan task fills.
    ///
    /// A separate step rather than a [`Self::new`] parameter because the
    /// registry only exists when a scan does: the daemon is built the same way
    /// either way, and a dry-run daemon — which constructs no scan at all —
    /// does not have to name a registry it will never have.
    #[must_use]
    pub fn with_service_registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Write the service registry into the live-state tree under
    /// [`crate::scan::STATE_KEY`], replacing it wholesale.
    ///
    /// The registry is rendered as one value rather than patched key by key so
    /// that `instance_collision`, which is a property of the whole table
    /// rather than of one entry, is never observable half-applied.
    pub async fn publish_services(&self, services: Value) {
        let mut inner = self.inner.write().await;
        if let Some(root) = inner.state.as_object_mut() {
            root.insert(crate::scan::STATE_KEY.to_string(), services);
        }
        drop(inner);
    }

    /// Clones of settings and live state for unit-test assertions.
    #[cfg(test)]
    pub async fn trees(&self) -> (Value, Value) {
        let inner = self.inner.read().await;
        let settings = inner.settings.get("").unwrap_or(Value::Null);
        (settings, inner.state.clone())
    }
}
