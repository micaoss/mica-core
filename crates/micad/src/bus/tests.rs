use super::MicadService;
use crate::deployment::{DeploymentClient, Status};
use crate::power::MockPower;
use std::sync::{Arc, Mutex};

mod apply;
mod auto;
mod bluetooth;
mod features;
mod install;
mod observers;
mod power;
mod refusals;
mod state;
mod update_config;
mod wireguard;
use observers::*;
use power::*;
use refusals::*;

struct MockDeployments {
    calls: CallLog,
    status: Status,
    install_error: Option<String>,
    install_gate: Option<Arc<tokio::sync::Notify>>,
    queries_fail: bool,
}

impl Default for MockDeployments {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            status: native_status(),
            install_error: None,
            install_gate: None,
            queries_fail: false,
        }
    }
}

fn native_status() -> Status {
    Status::parse(&crate::deployment::tests::fixture().to_string()).unwrap()
}

fn pending_status() -> Status {
    let mut status = native_status();
    let mut candidate = status.deployments[0].clone();
    candidate.id = "e".repeat(64);
    candidate.generation = 3;
    candidate.tries_left = Some(3);
    candidate.file = format!("mica-{}+3.conf", candidate.id);
    status.state.candidate = Some(candidate.id.clone());
    status.state.highest_generation = 3;
    status.deployments.push(candidate);
    status
}

#[async_trait::async_trait]
impl DeploymentClient for MockDeployments {
    async fn status(&self) -> anyhow::Result<Status> {
        anyhow::ensure!(!self.queries_fail, "native backend unreachable");
        Ok(self.status.clone())
    }
    async fn install(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("install {}", path.display()));
        if let Some(gate) = &self.install_gate {
            gate.notified().await;
        }
        if let Some(error) = &self.install_error {
            anyhow::bail!("{error}");
        }
        Ok(())
    }
    async fn confirm(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("confirm".into());
        Ok(())
    }
    async fn reject(&self, id: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("reject {id}"));
        Ok(())
    }
    async fn rollback(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("rollback".into());
        Ok(())
    }
}

struct RecordingReconciler {
    name: &'static str,
    subtree: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
}

struct BlockingReconciler {
    name: &'static str,
    subtree: &'static str,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl crate::reconciler::Reconciler for BlockingReconciler {
    fn name(&self) -> &'static str {
        self.name
    }

    fn subtree(&self) -> &'static str {
        self.subtree
    }

    async fn apply(
        &self,
        _settings: &micad_settings::Settings,
    ) -> anyhow::Result<serde_json::Value> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(serde_json::json!({"applied": true}))
    }
}

#[async_trait::async_trait]
impl crate::reconciler::Reconciler for RecordingReconciler {
    fn name(&self) -> &'static str {
        self.name
    }

    fn subtree(&self) -> &'static str {
        self.subtree
    }

    async fn apply(
        &self,
        _settings: &micad_settings::Settings,
    ) -> anyhow::Result<serde_json::Value> {
        self.calls
            .lock()
            .expect("recording reconciler call log")
            .push(self.name.to_string());
        Ok(serde_json::json!({"applied": true}))
    }
}

/// Three accounts, nine fields each — the shape of a Debian `/etc/shadow`.
const SHADOW: &str = "root:!:19000:0:99999:7:::\n\
    daemon:*:19000:0:99999:7:::\n";

/// A mock's shared call log ([`MockPower::calls`] / [`MockDeployments::calls`]).
type CallLog = Arc<Mutex<Vec<String>>>;

/// A store over a throwaway tree: the STATE document and the
/// `/mica/config/` namespace beside it.
fn store_in(dir: &tempfile::TempDir) -> micad_settings::Store {
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).expect("create the config namespace");
    micad_settings::Store::new(dir.path().join("settings.toml"), config)
}

/// Service backed by a throwaway settings file, a throwaway shadow file,
/// a recording power mock and the given the native backend mock; the power log and the
/// the native backend call log are returned alongside.
fn service_with_deployments(
    native: MockDeployments,
) -> (MicadService, CallLog, CallLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store_in(&dir);
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let deployment_calls = Arc::clone(&native.calls);
    let service = MicadService::new(
        store,
        micad_settings::Settings::default(),
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::clone(&calls),
        }),
        shadow_path,
        serde_json::json!({}),
    )
    .with_deployments(Arc::new(native))
    // Installs are admitted only from <workspace>/verified; the tests'
    // descriptors are placed there by `verified_descriptor`.
    .with_update_workspace(dir.path().join("updates"));
    (service, calls, deployment_calls, dir)
}

/// A descriptor file inside the test service's `verified/`, as a string path.
fn verified_descriptor(dir: &tempfile::TempDir, name: &str) -> String {
    let verified = dir.path().join("updates").join("verified");
    std::fs::create_dir_all(&verified).expect("verified/");
    let descriptor = verified.join(name);
    std::fs::write(&descriptor, b"descriptor bytes").expect("seed descriptor");
    descriptor.to_str().expect("utf-8").to_string()
}

/// [`service_with_deployments`] over a default native backend mock.
fn service_with_mock() -> (MicadService, Arc<Mutex<Vec<String>>>, tempfile::TempDir) {
    let (service, calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    (service, calls, dir)
}

/// A service with one reconciler for each subtree that makes an accidental
/// `apply_all` visible in the call log.
fn service_with_recording_reconcilers() -> (MicadService, CallLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let mut settings = micad_settings::Settings::default();
    settings.network.insert(
        "wg0".to_string(),
        micad_settings::IfaceSettings {
            kind: micad_settings::IfaceKind::Wireguard,
            wireguard: Some(micad_settings::WireguardConfig::default()),
            ..micad_settings::IfaceSettings::default()
        },
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let reconciler = |name, subtree| {
        Box::new(RecordingReconciler {
            name,
            subtree,
            calls: Arc::clone(&calls),
        }) as Box<dyn crate::reconciler::Reconciler>
    };
    let service = MicadService::new(
        store_in(&dir),
        settings,
        vec![
            reconciler("sshd", "access.ssh"),
            reconciler("network", "network"),
            reconciler("container", "container"),
        ],
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path,
        serde_json::json!({}),
    )
    .with_wireguard(Arc::new(crate::reconciler::network::KeyRotation::new(
        crate::wgkeys::Keystore::under(dir.path(), None),
        crate::reconciler::network::NoDelete,
    )));
    (service, calls, dir)
}

/// A mica-containerd that records the verbs it was asked for, refuses
/// nothing, and lists the one declared container as running.
struct RecordingDaemon {
    calls: CallLog,
}

#[async_trait::async_trait]
impl crate::containerd::Containerd for RecordingDaemon {
    async fn status(&self) -> Result<serde_json::Value, crate::containerd::ContainerdError> {
        Ok(serde_json::json!({}))
    }
    async fn declare(
        &self,
        _units: &std::collections::BTreeMap<String, micad_settings::ContainerUnit>,
    ) -> Result<serde_json::Value, crate::containerd::ContainerdError> {
        Ok(serde_json::json!({ "containers": [] }))
    }
    async fn containers(&self) -> Result<serde_json::Value, crate::containerd::ContainerdError> {
        Ok(serde_json::json!({ "containers": [
            { "spec": { "name": "node-red" }, "phase": "running", "health": "healthy", "restarts": 0 }
        ] }))
    }
    async fn act(
        &self,
        name: &str,
        verb: crate::containerd::Verb,
    ) -> Result<serde_json::Value, crate::containerd::ContainerdError> {
        self.calls
            .lock()
            .expect("call log")
            .push(format!("{} {name}", format!("{verb:?}").to_lowercase()));
        Ok(serde_json::json!({}))
    }
}

/// A service holding one declared container, with `enabled` as given.
fn service_with_container(enabled: bool) -> (MicadService, CallLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let mut settings = micad_settings::Settings::default();
    settings.container.enabled = enabled;
    settings.container.units.insert(
        "node-red".to_string(),
        micad_settings::ContainerUnit {
            image: "docker.io/nodered/node-red:4.0.9".to_string(),
            command: Vec::new(),
            environment: std::collections::BTreeMap::new(),
            publish: Vec::new(),
            volumes: Vec::new(),
            restart: micad_settings::RestartPolicy::default(),
            auto_start: true,
            pids: None,
            memory: None,
            cpu: None,
        },
    );
    let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path,
        serde_json::json!({}),
    )
    .with_containers(
        Arc::new(crate::containers::NoEngine),
        Arc::new(RecordingDaemon {
            calls: Arc::clone(&calls),
        }),
    );
    (service, calls, dir)
}

/// An adapter that records what the bus asked it to do.
#[derive(Default)]
struct MockBluetooth {
    calls: std::sync::Mutex<Vec<String>>,
    devices: Vec<crate::bluetooth::Device>,
    present: bool,
}

#[async_trait::async_trait]
impl crate::bluetooth::BluetoothControl for MockBluetooth {
    async fn adapter(&self) -> anyhow::Result<crate::bluetooth::Adapter> {
        if self.present {
            Ok(crate::bluetooth::Adapter {
                address: "11:22:33:44:55:66".to_string(),
                powered: true,
                ..crate::bluetooth::Adapter::default()
            })
        } else {
            anyhow::bail!("bluetoothd reports no adapter on this device")
        }
    }

    async fn devices(&self) -> anyhow::Result<Vec<crate::bluetooth::Device>> {
        Ok(self.devices.clone())
    }

    async fn set_discovery(&self, on: bool) -> anyhow::Result<()> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("discovery {on}"));
        Ok(())
    }

    async fn pair(&self, address: &str) -> anyhow::Result<()> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("pair {address}"));
        Ok(())
    }

    async fn remove(&self, address: &str) -> anyhow::Result<()> {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("remove {address}"));
        Ok(())
    }
}

/// A service with Bluetooth attached, switched on or off.
fn service_with_bluetooth(
    enabled: bool,
    present: bool,
) -> (MicadService, Arc<MockBluetooth>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let mut settings = micad_settings::Settings::default();
    settings.bluetooth.enabled = enabled;
    settings.provisioning.device_id = Some("0123456789abcdef0123456789abcdef".to_string());
    let adapter = Arc::new(MockBluetooth {
        present,
        devices: vec![crate::bluetooth::Device {
            address: "AA:BB:CC:DD:EE:01".to_string(),
            name: "phone".to_string(),
            paired: true,
            ..crate::bluetooth::Device::default()
        }],
        ..MockBluetooth::default()
    });
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path,
        serde_json::json!({}),
    )
    .with_bluetooth(
        Arc::clone(&adapter) as Arc<dyn crate::bluetooth::BluetoothControl>,
        Arc::new(crate::bluetooth::Agent::default()),
    );
    (service, adapter, dir)
}
