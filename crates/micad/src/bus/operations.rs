//! The work behind the larger bus methods: health reports, WireGuard keys, Bluetooth and Wi-Fi scans.

use serde_json::Value;
use zbus::fdo;

use super::*;

impl MicadService {
    /// The work behind `report_health` on the bus.
    pub(super) async fn record_health_report(
        &self,
        component: &str,
        status: &str,
        detail: &str,
    ) -> fdo::Result<()> {
        // The live-state tree lives in RAM for the life of the daemon, and
        // this is its only write surface that accepts arbitrary keys and
        // strings with no pruning. Callers are root-only, so the caps guard
        // against a wedged or looping reporter, not an attacker — but a root
        // daemon that can be grown without bound by a misbehaving oneshot is
        // still a daemon that eventually takes the device down with it.
        const MAX_COMPONENT_LEN: usize = 64;
        const MAX_STATUS_LEN: usize = 64;
        const MAX_DETAIL_LEN: usize = 1024;
        const MAX_COMPONENTS: usize = 128;
        if component.is_empty() {
            return Err(fdo::Error::InvalidArgs(
                "component must not be empty".into(),
            ));
        }
        if component.len() > MAX_COMPONENT_LEN
            || status.len() > MAX_STATUS_LEN
            || detail.len() > MAX_DETAIL_LEN
        {
            return Err(fdo::Error::InvalidArgs(format!(
                "health report too large: component <= {MAX_COMPONENT_LEN}, \
                 status <= {MAX_STATUS_LEN}, detail <= {MAX_DETAIL_LEN} bytes"
            )));
        }
        let mut inner = self.inner.write().await;
        if let Some(root) = inner.state.as_object_mut() {
            let health = root
                .entry("health")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(health) = health.as_object_mut() {
                if !health.contains_key(component) && health.len() >= MAX_COMPONENTS {
                    return Err(fdo::Error::InvalidArgs(format!(
                        "health table already holds {MAX_COMPONENTS} components; \
                         refusing a new one"
                    )));
                }
                health.insert(
                    component.to_string(),
                    serde_json::json!({ "status": status, "detail": detail }),
                );
            }
        }
        drop(inner);
        tracing::info!(component, status, detail, "health report recorded");
        Ok(())
    }

    /// The work behind `rotate_wireguard_key` on the bus.
    pub(super) async fn rotate_wireguard_key_now(
        &self,
        iface: &str,
    ) -> Result<String, SettingsFault> {
        // Under the same lock every mutating method takes: two unserialized
        // rotations of one interface would each write a key and each delete
        // the device, and the public key one of them returned would be the
        // half of a private key the other had already replaced.
        let _apply = self.apply_lock.lock().await;
        let settings = self.inner.read().await.settings.clone();
        match settings.network.get(iface) {
            Some(cfg) if cfg.kind == micad_settings::IfaceKind::Wireguard => {}
            // The entry exists and its kind is wrong: a bad argument, and the
            // 422 apid already answers for one.
            Some(_) => {
                return Err(SettingsFault::Fdo(fdo::Error::InvalidArgs(format!(
                    "network.{iface} is not a WireGuard interface"
                ))));
            }
            // The entry does not exist: the path names nothing, which is the
            // condition every other read on this bus already raises
            // [`NOT_FOUND_ERROR`] for and which apid already maps to 404.
            // These two travelled under one error name until later, and
            // that is why the shipped rotate-key route answered 422 where the
            // settings and state reads beside it answered 404 for the same
            // class of condition. The
            // split is here rather than in apid because apid had nothing left
            // to tell them apart with.
            None => {
                return Err(SettingsFault::NotFound(format!(
                    "network.{iface} is not a declared network entry"
                )));
            }
        }
        let public_key = self
            .wireguard
            .rotate_key(iface)
            .await
            // The anyhow chain names paths and never key material: the key
            // store's errors are written that way, and this adds no value of
            // its own to them.
            .map_err(|err| {
                SettingsFault::Fdo(fdo::Error::Failed(format!("rotate wireguard key: {err:#}")))
            })?;
        self.apply_subtree("network").await;
        Ok(public_key)
    }

    /// The work behind `pair_bluetooth_device` on the bus.
    pub(super) async fn pair_bluetooth_device_now(&self, address: &str) -> fdo::Result<()> {
        self.require_bluetooth().await?;
        if !micad_settings::is_bluetooth_address(address) {
            return Err(fdo::Error::InvalidArgs(format!(
                "{address:?} is not a Bluetooth address"
            )));
        }
        self.bluetooth
            .pair(address)
            .await
            .map_err(|err| fdo::Error::Failed(format!("pair {address}: {err:#}")))?;

        // Under the apply lock, like every settings write: a pair landing
        // during a reconcile must not be lost to the tree that reconcile wrote.
        let _apply = self.apply_lock.lock().await;
        let name = self
            .bluetooth
            .devices()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|device| device.address == address)
            .map(|device| device.name)
            .unwrap_or_default();
        let mut devices = self.inner.read().await.settings.bluetooth.devices.clone();
        devices.insert(
            address.to_string(),
            micad_settings::PairedDevice {
                name,
                // Trusted on pairing: an operator who just confirmed a passkey
                // has said yes to this device, and a paired-but-untrusted
                // device asks again on every reconnect.
                trusted: true,
                blocked: false,
            },
        );
        let value = serde_json::to_value(&devices).unwrap_or(Value::Null);
        self.persist_setting("bluetooth.devices", value)
            .await
            .map_err(|err| fdo::Error::Failed(format!("persist the trust list: {err}")))?;
        drop(_apply);
        self.apply_subtree("bluetooth").await;
        Ok(())
    }

    /// The work behind `scan_wifi` on the bus.
    pub(super) async fn scan_wifi_now(&self) -> Result<String, SettingsFault> {
        self.require_feature(micad_settings::Feature::Wifi)
            .map_err(SettingsFault::Fdo)?;
        let settings = self.inner.read().await.settings.clone();
        let interface = settings.wifi.client.interface.clone();
        let root = std::path::PathBuf::from("/");
        let scanned = tokio::task::spawn_blocking(move || {
            crate::network_state::scan_networks(&root, &interface)
        })
        .await
        .map_err(|err| SettingsFault::Fdo(fdo::Error::Failed(format!("wifi scan task: {err}"))))?;
        let value = match scanned {
            Ok(networks) => serde_json::json!({
                "available": true,
                "interface": settings.wifi.client.interface,
                "networks": networks
                    .iter()
                    .map(|network| serde_json::json!({
                        "ssid": network.ssid,
                        "bssid": network.bssid,
                        "frequencyMhz": network.frequency_mhz,
                        "signalDbm": network.signal_dbm,
                        "flags": network.flags,
                    }))
                    .collect::<Vec<_>>(),
            }),
            Err(detail) => serde_json::json!({
                "available": false,
                "interface": settings.wifi.client.interface,
                "detail": detail,
            }),
        };
        Ok(value.to_string())
    }

    /// The work behind `get_bluetooth` on the bus.
    pub(super) async fn bluetooth_view(&self) -> Result<String, SettingsFault> {
        self.require_feature(micad_settings::Feature::Bluetooth)
            .map_err(SettingsFault::Fdo)?;
        let settings = self.inner.read().await.settings.clone();
        let pin = self.pairing_pin(&settings);
        let mut value = crate::bluetooth::observed_json(
            &settings.bluetooth.devices,
            self.bluetooth
                .adapter()
                .await
                .map_err(|err| format!("{err:#}")),
            self.bluetooth
                .devices()
                .await
                .map_err(|err| format!("{err:#}")),
            &pin,
        );
        value["enabled"] = serde_json::json!(settings.bluetooth.enabled);
        value["discoverable"] = serde_json::json!(settings.bluetooth.discoverable);
        // What the console renders its pairing dialog from. Absent when
        // nothing is waiting, which is most of the time.
        value["pending"] = match self.agent.pending().await {
            Some(pending) => serde_json::to_value(pending).unwrap_or(Value::Null),
            None => Value::Null,
        };
        Ok(value.to_string())
    }
}
