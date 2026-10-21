//! Reconciler for the `bluetooth` settings subtree.
//!
//! It owns the adapter and the trust list, and nothing else. Pairing is an
//! action with an operator in it (`bus.rs`), and a reconcile pass is not a
//! place to wait for a person.
//!
//! The subject is **optional hardware**, which no other reconciler here has.
//! A board with no radio must not grow a failing unit: when there is no
//! adapter the pass reports `unsupported` with the reason and touches nothing.
//!
//! It also runs `mica-bled`, the phone app's way in over Bluetooth LE, when
//! the root carries it: running while Bluetooth is on and the adapter is
//! there, stopped otherwise. The package ships the unit; micad only starts it
//! by name, and writes the one file it reads.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use micad_settings::Settings;

use super::Reconciler;
use super::systemd::{UnitControl, is_active};
use crate::bluetooth::{BluetoothControl, Device};
use crate::fswrite::write_config_if_changed;

/// The unit BlueZ ships.
pub const BLUETOOTH_UNIT: &str = "bluetooth.service";

/// The unit mica-pkgs' `mica-bled` ships; OpenRC's script has the same stem.
const BLED_UNIT: &str = "mica-bled.service";
/// The binary whose presence says the root carries `mica-bled`: the same
/// answer under either init, and no unit lookup that fails on a root without.
const BLED_BINARY: &str = "/usr/bin/mica-bled";
/// What `mica-bled` is started with: the device id and apid's local socket.
/// Its unit does not start without it.
const BLED_ENV: &str = "/run/mica/bled.env";
/// The device id is not a secret, and init reads the file as root anyway.
const BLED_ENV_MODE: u32 = 0o644;
/// `ActiveState` of a unit systemd has given up on.
const FAILED_STATE: &str = "failed";

/// Where `mica-bled` is looked for and its environment written.
struct Bled {
    binary: PathBuf,
    env: PathBuf,
}

/// Reconciler for the `bluetooth` subtree.
pub struct BluetoothReconciler<C: UnitControl> {
    /// BlueZ's unit, or on an OpenRC root the Bluetooth package's
    /// `mica-bluetoothd`.
    unit: &'static str,
    control: C,
    adapter: Arc<dyn BluetoothControl>,
    bled: Bled,
}

impl<C: UnitControl> BluetoothReconciler<C> {
    /// Reconciler driving `control` for the unit and `adapter` for BlueZ.
    pub fn new(control: C, adapter: Arc<dyn BluetoothControl>) -> Self {
        Self {
            unit: BLUETOOTH_UNIT,
            control,
            adapter,
            bled: Bled {
                binary: PathBuf::from(BLED_BINARY),
                env: PathBuf::from(BLED_ENV),
            },
        }
    }

    /// The same reconciler looking for `mica-bled` at `binary` and writing its
    /// environment to `env`, so tests stay inside a temporary directory.
    #[cfg(test)]
    fn with_bled(mut self, binary: PathBuf, env: PathBuf) -> Self {
        self.bled = Bled { binary, env };
        self
    }

    /// The same reconciler over `mica-bluetoothd`, bluetoothd on an OpenRC
    /// root.
    pub fn openrc(control: C, adapter: Arc<dyn BluetoothControl>) -> Self {
        Self {
            unit: "mica-bluetoothd.service",
            ..Self::new(control, adapter)
        }
    }

    /// Bring the declared devices to their declared flags, and remove the
    /// paired ones nobody declares.
    ///
    /// **Only paired devices are removed.** BlueZ publishes an object for
    /// every device it has merely seen, and sweeping those would delete the
    /// results of the scan an operator is looking at. A paired device that the
    /// settings tree does not name is the one this reconciler is responsible
    /// for: the declaration is the trust list, so an entry that left it must
    /// leave the adapter too.
    async fn reconcile_devices(&self, settings: &Settings) -> (Vec<String>, Vec<String>) {
        let declared = &settings.bluetooth.devices;
        let mut adjusted = Vec::new();
        let mut removed = Vec::new();
        let known: Vec<Device> = self.adapter.devices().await.unwrap_or_default();
        for device in &known {
            match declared.get(&device.address) {
                Some(wanted) => {
                    if device.trusted != wanted.trusted
                        && self
                            .adapter
                            .set_trusted(&device.address, wanted.trusted)
                            .await
                            .is_ok()
                    {
                        adjusted.push(device.address.clone());
                    }
                    if device.blocked != wanted.blocked {
                        let _ = self
                            .adapter
                            .set_blocked(&device.address, wanted.blocked)
                            .await;
                    }
                }
                None if device.paired && self.adapter.remove(&device.address).await.is_ok() => {
                    removed.push(device.address.clone());
                }
                None => {}
            }
        }
        adjusted.sort();
        removed.sort();
        (adjusted, removed)
    }
}

impl<C: UnitControl> BluetoothReconciler<C> {
    /// Run `mica-bled` against the current device id, restarting it when its
    /// environment changed. What it reports, rather than an error: the
    /// adapter is applied either way, and a phone service that does not start
    /// must not make the adapter's pass fail.
    async fn bled_on(&self, settings: &Settings) -> serde_json::Value {
        if !self.bled.binary.exists() {
            return serde_json::json!({ "outcome": "absent" });
        }
        let result = async {
            let device_id = settings
                .provisioning
                .device_id
                .as_deref()
                .filter(|id| micad_settings::validate_device_id(id).is_ok())
                .ok_or_else(|| anyhow::anyhow!("the device id is not provisioned"))?;
            let env = format!(
                "MICA_BLED_DEVICE_ID={device_id}\nMICA_BLED_APID={}\n",
                micad_settings::APID_LOCAL_SOCKET_PATH
            );
            let changed = write_config_if_changed(&self.bled.env, &env, BLED_ENV_MODE)?;
            let state = self.control.active_state(BLED_UNIT).await?;
            if is_active(&state) {
                if changed {
                    self.control.restart(BLED_UNIT).await?;
                }
            } else {
                if state == FAILED_STATE {
                    self.control.reset_failed(BLED_UNIT).await?;
                }
                self.control.start(BLED_UNIT).await?;
            }
            anyhow::Ok(())
        }
        .await;
        match result {
            Ok(()) => serde_json::json!({ "outcome": "running", "unit": BLED_UNIT }),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "bluetooth: mica-bled did not start");
                serde_json::json!({
                    "outcome": "failed",
                    "unit": BLED_UNIT,
                    "detail": format!("{err:#}"),
                })
            }
        }
    }

    /// Stop `mica-bled` and take its environment away, so nothing starts it
    /// against a switch that is off.
    async fn bled_off(&self) -> serde_json::Value {
        if !self.bled.binary.exists() {
            return serde_json::json!({ "outcome": "absent" });
        }
        let result = async {
            if is_active(&self.control.active_state(BLED_UNIT).await?) {
                self.control.stop(BLED_UNIT).await?;
            }
            match std::fs::remove_file(&self.bled.env) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
                _ => anyhow::Ok(()),
            }
        }
        .await;
        match result {
            Ok(()) => serde_json::json!({ "outcome": "stopped", "unit": BLED_UNIT }),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "bluetooth: mica-bled did not stop");
                serde_json::json!({
                    "outcome": "failed",
                    "unit": BLED_UNIT,
                    "detail": format!("{err:#}"),
                })
            }
        }
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for BluetoothReconciler<C> {
    fn name(&self) -> &'static str {
        "bluetooth"
    }

    fn subtree(&self) -> &'static str {
        "bluetooth"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let wanted = &settings.bluetooth;
        // A provisioning window powers the adapter even when Bluetooth is
        // switched off, and gives it back when it closes.
        if !wanted.enabled && !crate::ble_provisioning::radio_held() {
            // mica-bled first: it is bluetoothd's client.
            let bled = self.bled_off().await;
            // Stopped, not disabled: the unit ships enabled with the package
            // and enablement is not this switch's business. What the switch
            // decides is whether the adapter is running now.
            if is_active(&self.control.active_state(self.unit).await?) {
                self.control.stop(self.unit).await?;
            }
            return Ok(serde_json::json!({
                "outcome": "disabled",
                "unit": self.unit,
                "bled": bled,
            }));
        }

        if !is_active(&self.control.active_state(self.unit).await?) {
            self.control.start(self.unit).await?;
        }
        // The adapter is asked for AFTER the unit is up: bluetoothd publishes
        // nothing before it runs, and a board with a radio would otherwise
        // look like a board without one on the first pass of a boot.
        let adapter = match self.adapter.adapter().await {
            Ok(adapter) => adapter,
            Err(err) => {
                // Not an error: a board with no radio is a board doing what it
                // is, and a reconciler that failed here would fail on every
                // pass forever.
                return Ok(serde_json::json!({
                    "outcome": "unsupported",
                    "unit": self.unit,
                    "detail": format!("{err:#}"),
                    "bled": self.bled_off().await,
                }));
            }
        };

        if !adapter.powered {
            self.adapter.set_powered(true).await?;
        }
        // Whether it is discoverable and pairable is the pairing window's
        // (`crate::bluetooth::Agent`), never a setting.
        // The adapter advertises the hostname: a device on the air should be
        // recognisable as the device it is, and the hostname is the name its
        // operator already chose.
        let alias = settings.hostname.clone();
        if adapter.alias != alias {
            self.adapter.set_alias(&alias).await?;
        }
        let (adjusted, removed) = self.reconcile_devices(settings).await;
        let bled = if wanted.enabled {
            self.bled_on(settings).await
        } else {
            self.bled_off().await
        };

        Ok(serde_json::json!({
            "outcome": "applied",
            "unit": self.unit,
            "address": adapter.address,
            "alias": alias,
            "declaredCount": wanted.devices.len(),
            "adjustedDevices": adjusted,
            "removedDevices": removed,
            "bled": bled,
        }))
    }
}

#[cfg(test)]
mod tests;
