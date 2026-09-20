//! Reconciler for the `bluetooth` settings subtree.
//!
//! It owns the adapter and the trust list, and nothing else. Pairing is an
//! action with an operator in it (`bus.rs`), and a reconcile pass is not a
//! place to wait for a person.
//!
//! The subject is **optional hardware**, which no other reconciler here has.
//! A board with no radio must not grow a failing unit: when there is no
//! adapter the pass reports `unsupported` with the reason and touches nothing.

use std::sync::Arc;

use anyhow::Result;
use micad_settings::Settings;

use super::Reconciler;
use super::systemd::{UnitControl, is_active};
use crate::bluetooth::{BluetoothControl, Device};

/// The unit BlueZ ships.
pub const BLUETOOTH_UNIT: &str = "bluetooth.service";

/// Reconciler for the `bluetooth` subtree.
pub struct BluetoothReconciler<C: UnitControl> {
    control: C,
    adapter: Arc<dyn BluetoothControl>,
}

impl<C: UnitControl> BluetoothReconciler<C> {
    /// Reconciler driving `control` for the unit and `adapter` for BlueZ.
    pub fn new(control: C, adapter: Arc<dyn BluetoothControl>) -> Self {
        Self { control, adapter }
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
        if !wanted.enabled {
            // Stopped, not disabled: the unit ships enabled with the package
            // and enablement is not this switch's business. What the switch
            // decides is whether the adapter is running now.
            if is_active(&self.control.active_state(BLUETOOTH_UNIT).await?) {
                self.control.stop(BLUETOOTH_UNIT).await?;
            }
            return Ok(serde_json::json!({
                "outcome": "disabled",
                "unit": BLUETOOTH_UNIT,
            }));
        }

        if !is_active(&self.control.active_state(BLUETOOTH_UNIT).await?) {
            self.control.start(BLUETOOTH_UNIT).await?;
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
                    "unit": BLUETOOTH_UNIT,
                    "detail": format!("{err:#}"),
                }));
            }
        };

        if !adapter.powered {
            self.adapter.set_powered(true).await?;
        }
        if adapter.discoverable != wanted.discoverable {
            self.adapter.set_discoverable(wanted.discoverable).await?;
        }
        // An absent alias advertises the hostname: a device on the air should
        // be recognisable as the device it is, and the hostname is the name
        // its operator already chose.
        let alias = wanted
            .alias
            .clone()
            .unwrap_or_else(|| settings.hostname.clone());
        if adapter.alias != alias {
            self.adapter.set_alias(&alias).await?;
        }
        let (adjusted, removed) = self.reconcile_devices(settings).await;

        Ok(serde_json::json!({
            "outcome": "applied",
            "unit": BLUETOOTH_UNIT,
            "address": adapter.address,
            "alias": alias,
            "discoverable": wanted.discoverable,
            "declaredCount": wanted.devices.len(),
            "adjustedDevices": adjusted,
            "removedDevices": removed,
        }))
    }
}

#[cfg(test)]
mod tests;
