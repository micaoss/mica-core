use super::*;
use crate::bluetooth::Adapter;
use crate::reconciler::systemd::mock::MockUnitControl;
use std::sync::Mutex;

/// A BlueZ that answers from a fixture and records what it was asked to do.
#[derive(Default)]
struct MockAdapter {
    adapter: Option<Adapter>,
    devices: Vec<Device>,
    calls: Mutex<Vec<String>>,
}

impl MockAdapter {
    fn with_adapter(adapter: Adapter, devices: Vec<Device>) -> Arc<Self> {
        Arc::new(Self {
            adapter: Some(adapter),
            devices,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("call log").push(call);
    }
}

#[async_trait::async_trait]
impl BluetoothControl for MockAdapter {
    async fn adapter(&self) -> Result<Adapter> {
        self.adapter
            .clone()
            .ok_or_else(|| anyhow::anyhow!("bluetoothd reports no adapter on this device"))
    }

    async fn devices(&self) -> Result<Vec<Device>> {
        Ok(self.devices.clone())
    }

    async fn set_powered(&self, on: bool) -> Result<()> {
        self.record(format!("powered {on}"));
        Ok(())
    }

    async fn set_discoverable(&self, on: bool) -> Result<()> {
        self.record(format!("discoverable {on}"));
        Ok(())
    }

    async fn set_alias(&self, alias: &str) -> Result<()> {
        self.record(format!("alias {alias}"));
        Ok(())
    }

    async fn set_trusted(&self, address: &str, trusted: bool) -> Result<()> {
        self.record(format!("trusted {address} {trusted}"));
        Ok(())
    }

    async fn set_blocked(&self, address: &str, blocked: bool) -> Result<()> {
        self.record(format!("blocked {address} {blocked}"));
        Ok(())
    }

    async fn remove(&self, address: &str) -> Result<()> {
        self.record(format!("remove {address}"));
        Ok(())
    }
}

fn settings(enabled: bool) -> Settings {
    let mut settings = Settings {
        hostname: "edge-42".to_string(),
        ..Default::default()
    };
    settings.bluetooth.enabled = enabled;
    settings
}

fn declare(settings: &mut Settings, address: &str, trusted: bool, blocked: bool) {
    settings.bluetooth.devices.insert(
        address.to_string(),
        micad_settings::PairedDevice {
            name: "phone".to_string(),
            trusted,
            blocked,
        },
    );
}

fn powered_adapter() -> Adapter {
    Adapter {
        address: "11:22:33:44:55:66".to_string(),
        alias: "edge-42".to_string(),
        powered: true,
        discoverable: false,
        discovering: false,
    }
}

/// The switch off stops the unit and asks BlueZ nothing: there is nothing to
/// ask, and a stopped daemon would refuse anyway.
#[tokio::test]
async fn the_switch_off_stops_the_unit_and_touches_no_adapter() {
    let adapter = MockAdapter::with_adapter(powered_adapter(), Vec::new());
    let control = MockUnitControl::new("active", "enabled");
    let reconciler =
        BluetoothReconciler::new(control, Arc::clone(&adapter) as Arc<dyn BluetoothControl>);

    let state = reconciler.apply(&settings(false)).await.expect("apply");

    assert_eq!(state["outcome"], serde_json::json!("disabled"));
    assert!(adapter.calls().is_empty());
}

/// A board with no radio is a board doing what it is. The pass reports it and
/// does not fail, because a failing reconciler fails on every pass forever.
#[tokio::test]
async fn a_board_without_an_adapter_is_unsupported_and_not_a_failure() {
    let adapter: Arc<MockAdapter> = Arc::new(MockAdapter::default());
    let control = MockUnitControl::new("inactive", "enabled");
    let reconciler =
        BluetoothReconciler::new(control, Arc::clone(&adapter) as Arc<dyn BluetoothControl>);

    let state = reconciler.apply(&settings(true)).await.expect("apply");

    assert_eq!(state["outcome"], serde_json::json!("unsupported"));
    assert!(
        state["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no adapter"))
    );
}

/// An absent alias advertises the hostname: a device on the air should be
/// recognisable as the device it is.
#[tokio::test]
async fn an_undeclared_alias_advertises_the_hostname() {
    let adapter = MockAdapter::with_adapter(
        Adapter {
            alias: "BlueZ 5.79".to_string(),
            ..powered_adapter()
        },
        Vec::new(),
    );
    let control = MockUnitControl::new("active", "enabled");
    let reconciler =
        BluetoothReconciler::new(control, Arc::clone(&adapter) as Arc<dyn BluetoothControl>);

    let state = reconciler.apply(&settings(true)).await.expect("apply");

    assert_eq!(state["alias"], serde_json::json!("edge-42"));
    assert!(adapter.calls().contains(&"alias edge-42".to_string()));
}

/// A converged adapter is asked for nothing: the properties already say what
/// the settings say.
#[tokio::test]
async fn a_converged_adapter_is_left_alone() {
    let adapter = MockAdapter::with_adapter(powered_adapter(), Vec::new());
    let control = MockUnitControl::new("active", "enabled");
    let reconciler =
        BluetoothReconciler::new(control, Arc::clone(&adapter) as Arc<dyn BluetoothControl>);

    let state = reconciler.apply(&settings(true)).await.expect("apply");

    assert_eq!(state["outcome"], serde_json::json!("applied"));
    assert!(adapter.calls().is_empty(), "{:?}", adapter.calls());
}

/// The declaration is the trust list: a declared device is brought to its
/// flags, and a paired device nobody declares leaves the adapter.
#[tokio::test]
async fn the_declaration_is_the_trust_list() {
    let adapter = MockAdapter::with_adapter(
        powered_adapter(),
        vec![
            Device {
                address: "AA:BB:CC:DD:EE:01".to_string(),
                paired: true,
                trusted: false,
                ..Device::default()
            },
            // Paired and not declared: this one goes.
            Device {
                address: "AA:BB:CC:DD:EE:02".to_string(),
                paired: true,
                ..Device::default()
            },
            // Merely seen by a scan: not this reconciler's to remove, or a
            // sweep would delete the results an operator is looking at.
            Device {
                address: "AA:BB:CC:DD:EE:03".to_string(),
                paired: false,
                ..Device::default()
            },
        ],
    );
    let control = MockUnitControl::new("active", "enabled");
    let reconciler =
        BluetoothReconciler::new(control, Arc::clone(&adapter) as Arc<dyn BluetoothControl>);
    let mut settings = settings(true);
    declare(&mut settings, "AA:BB:CC:DD:EE:01", true, false);

    let state = reconciler.apply(&settings).await.expect("apply");

    assert_eq!(
        state["adjustedDevices"],
        serde_json::json!(["AA:BB:CC:DD:EE:01"])
    );
    assert_eq!(
        state["removedDevices"],
        serde_json::json!(["AA:BB:CC:DD:EE:02"])
    );
    let calls = adapter.calls();
    assert!(
        calls.contains(&"trusted AA:BB:CC:DD:EE:01 true".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"remove AA:BB:CC:DD:EE:02".to_string()),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("AA:BB:CC:DD:EE:03")),
        "a device that was only seen was touched: {calls:?}"
    );
}
