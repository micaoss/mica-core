//! BLE provisioning's device side: what a session reads and changes, through
//! the same settings writes and observers the bus members use.

use std::time::Duration;

use micad_settings::{ApMode, ClaimChannel, ClaimSettings, Feature, WifiNetwork};
use serde_json::{Value, json};
use zbus::object_server::{InterfaceRef, SignalEmitter};

use super::*;
use crate::ble_provisioning::engine::{Device, Facts, StagedClaim};

/// The sender a provisioning write is recorded as.
const SENDER: &str = "ble-provisioning";
/// How long a `setWifi` waits for an address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long it waits for an address once associated.
const DHCP_TIMEOUT: Duration = Duration::from_secs(30);
/// How often it looks.
const CONNECT_POLL: Duration = Duration::from_secs(2);

/// `wpa_state`s that mean the key was being negotiated.
const HANDSHAKE_STATES: [&str; 2] = ["4WAY_HANDSHAKE", "GROUP_HANDSHAKE"];

impl MicadService {
    /// Persist `writes` and queue their apply, as `SetSettingsMany` does.
    async fn provisioning_write(
        &self,
        emitter: &SignalEmitter<'_>,
        writes: &[(String, Value)],
        claim: bool,
    ) -> Result<(), micad_settings::SettingsError> {
        if claim {
            self.persist_claim(writes).await?;
        } else {
            self.persist_settings(writes).await?;
        }
        for (path, value) in writes {
            let _ = Self::settings_changed(emitter, path, &value.to_string()).await;
        }
        let paths: Vec<String> = writes.iter().map(|(path, _)| path.clone()).collect();
        self.enqueue_apply(
            emitter,
            "settings-write",
            &interface::shared_path(&paths),
            SENDER,
        )
        .await;
        Ok(())
    }

    async fn provisioning_facts(&self) -> Facts {
        let settings = self.inner.read().await.settings.clone();
        let model = match self.system_info.observe().await {
            Ok(evidence) => evidence.board.map(|board| board.model),
            Err(_) => None,
        };
        Facts {
            claimed: settings.access.web_admin.is_some(),
            hostname: settings.hostname,
            model: model.unwrap_or_else(|| "mica".to_string()),
            auto_when_offline: settings.bluetooth.provisioning.auto_when_offline,
            has_wifi: self.features.has(Feature::Wifi),
        }
    }

    /// The station's association and address, from the network observer.
    async fn station(&self) -> (Option<String>, Option<String>, Option<String>) {
        let interface = self
            .inner
            .read()
            .await
            .settings
            .wifi
            .client
            .interface
            .clone();
        let Ok(observed) = self.network_state.observe().await else {
            return (None, None, None);
        };
        let association = observed
            .pointer("/wifi/associations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry["interface"] == interface.as_str());
        let state = association.and_then(|entry| entry["state"].as_str().map(str::to_string));
        let ssid = association
            .filter(|entry| entry["associated"] == true)
            .and_then(|entry| entry["ssid"].as_str().map(str::to_string));
        let ip = observed
            .pointer("/interfaces/entries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry["name"] == interface.as_str())
            .and_then(|entry| entry["addresses"].as_array())
            .into_iter()
            .flatten()
            .find(|address| address["family"] == "ipv4")
            .and_then(|address| address["address"].as_str().map(str::to_string));
        (state, ssid, ip)
    }

    async fn provisioning_status(&self) -> Value {
        let wifi = self.inner.read().await.settings.wifi.clone();
        let mode = if wifi.ap.mode != ApMode::Off {
            "ap"
        } else if wifi.client.enabled {
            "client"
        } else {
            "off"
        };
        let (_, ssid, ip) = if mode == "client" {
            self.station().await
        } else {
            (None, None, None)
        };
        json!({
            "wifiMode": mode,
            "connected": ssid.is_some() && ip.is_some(),
            "ssid": ssid,
            "ip": ip,
        })
    }

    async fn provisioning_scan(&self) -> Result<Vec<Value>, &'static str> {
        if !self.features.has(Feature::Wifi) {
            return Err("no_radio");
        }
        let scanned: Value = self
            .scan_wifi_now()
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or("scan_failed")?;
        if scanned["available"] != true {
            return Err(if scanned["reason"] == "access-point" {
                "ap_active"
            } else {
                "scan_failed"
            });
        }
        Ok(scanned["networks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|network| {
                json!({
                    "ssid": network["ssid"],
                    "signalDbm": network["signalDbm"],
                    "security": network["security"],
                    "channel": network["channel"],
                })
            })
            .collect())
    }

    fn wifi_snapshot_now(settings: &micad_settings::Settings) -> Value {
        json!({
            "clientEnabled": settings.wifi.client.enabled,
            "apMode": settings.wifi.ap.mode,
        })
    }

    async fn provisioning_join(
        &self,
        emitter: &SignalEmitter<'_>,
        ssid: &str,
        psk: &str,
        hidden: bool,
    ) -> Result<bool, &'static str> {
        let mut networks = self
            .inner
            .read()
            .await
            .settings
            .wifi
            .client
            .networks
            .clone();
        let top = networks.iter().map(|network| network.priority).max();
        let priority = top.map_or(0, |top| top.saturating_add(1));
        let joined = WifiNetwork {
            ssid: ssid.to_string(),
            psk: (!psk.is_empty()).then(|| psk.to_string()),
            hidden,
            priority,
            auto_connect: None,
        };
        let added = match networks.iter_mut().find(|network| network.ssid == ssid) {
            Some(network) => {
                *network = joined;
                false
            }
            None => {
                networks.push(joined);
                true
            }
        };
        let writes = [
            (
                "wifi.client.networks".to_string(),
                serde_json::to_value(&networks).map_err(|_| "connect_failed")?,
            ),
            ("wifi.client.enabled".to_string(), json!(true)),
            ("wifi.ap.mode".to_string(), json!("off")),
        ];
        self.provisioning_write(emitter, &writes, false)
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, "the provisioned network could not be written");
                "connect_failed"
            })?;
        Ok(added)
    }

    async fn provisioning_restore(
        &self,
        emitter: &SignalEmitter<'_>,
        snapshot: &Value,
        remove: &[String],
    ) {
        let mut networks = self
            .inner
            .read()
            .await
            .settings
            .wifi
            .client
            .networks
            .clone();
        networks.retain(|network| !remove.contains(&network.ssid));
        let Ok(networks) = serde_json::to_value(&networks) else {
            return;
        };
        let writes = [
            ("wifi.client.networks".to_string(), networks),
            (
                "wifi.client.enabled".to_string(),
                snapshot["clientEnabled"].clone(),
            ),
            ("wifi.ap.mode".to_string(), snapshot["apMode"].clone()),
        ];
        if let Err(err) = self.provisioning_write(emitter, &writes, false).await {
            tracing::warn!(error = %err, "the Wi-Fi a provisioning session changed could not be restored");
        }
    }

    async fn provisioning_claim(
        &self,
        emitter: &SignalEmitter<'_>,
        staged: &StagedClaim,
    ) -> Result<Option<String>, &'static str> {
        let password = staged.password.clone();
        let hash = tokio::task::spawn_blocking(move || micad_settings::hash_password(&password))
            .await
            .map_err(|_| "busy")?
            .map_err(|_| "busy")?;
        let mut access = self.inner.read().await.settings.access.clone();
        access.web_admin = Some(micad_settings::WebAdminSettings {
            password_hash: hash,
        });
        access.claim = Some(ClaimSettings {
            via: ClaimChannel::Ble,
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            rotation_required: false,
        });
        let wifi = self.features.has(Feature::Wifi);
        let generated = (wifi && staged.ap_psk.is_none()).then(micad_settings::generate_ap_psk);
        let mut writes = vec![(
            "access".to_string(),
            serde_json::to_value(&access).map_err(|_| "busy")?,
        )];
        if let Some(hostname) = &staged.hostname {
            writes.push(("hostname".to_string(), json!(hostname)));
        }
        if let Some(psk) = staged.ap_psk.as_ref().or(generated.as_ref()) {
            writes.push(("wifi.ap.psk".to_string(), json!(psk)));
        }
        match self.provisioning_write(emitter, &writes, true).await {
            Ok(()) => {}
            Err(micad_settings::SettingsError::Claimed) => return Err("already_claimed"),
            Err(err) => {
                tracing::warn!(error = %err, "the BLE claim could not be written");
                return Err("busy");
            }
        }
        if let Err(err) = self.provisioning.password_set(staged.password.clone()) {
            tracing::warn!(error = %format!("{err:#}"), "the BLE verifier could not be made at the claim");
        }
        self.provisioning.audit("claim", "completed");
        Ok(generated)
    }

    /// Re-run the Bluetooth reconciler: the radio was taken or given back.
    pub(crate) async fn apply_bluetooth(&self) {
        let _apply = self.apply_lock.lock().await;
        self.apply_subtree("bluetooth").await;
    }
}

/// Why a `setWifi` did not connect, from what the station went through.
fn connect_failure(seen: &[String], scanned: Option<&[Value]>, ssid: &str) -> &'static str {
    if seen
        .iter()
        .any(|state| HANDSHAKE_STATES.contains(&state.as_str()))
    {
        return "wrong_password";
    }
    if seen.iter().any(|state| state == "COMPLETED") {
        return "dhcp_timeout";
    }
    match scanned {
        Some(networks) => match networks.iter().find(|network| network["ssid"] == ssid) {
            None => "ssid_not_found",
            Some(network) if matches!(network["security"].as_str(), Some("wpa2-eap" | "wep")) => {
                "unsupported_security"
            }
            Some(_) => "connect_failed",
        },
        None => "connect_failed",
    }
}

#[async_trait::async_trait]
impl crate::ble_provisioning::bluez::Reconcile for InterfaceRef<MicadService> {
    async fn bluetooth(&self) {
        self.get().await.apply_bluetooth().await;
    }
}

#[async_trait::async_trait]
impl Device for InterfaceRef<MicadService> {
    async fn facts(&self) -> Facts {
        self.get().await.provisioning_facts().await
    }

    async fn status(&self) -> Value {
        self.get().await.provisioning_status().await
    }

    async fn scan(&self) -> Result<Vec<Value>, &'static str> {
        self.get().await.provisioning_scan().await
    }

    async fn wifi_snapshot(&self) -> Value {
        MicadService::wifi_snapshot_now(&self.get().await.inner.read().await.settings)
    }

    async fn join_wifi(&self, ssid: &str, psk: &str, hidden: bool) -> Result<bool, &'static str> {
        self.get()
            .await
            .provisioning_join(self.signal_emitter(), ssid, psk, hidden)
            .await
    }

    async fn await_wifi(&self, ssid: &str) -> Result<String, &'static str> {
        let started = tokio::time::Instant::now();
        let mut seen: Vec<String> = Vec::new();
        let mut associated_at = None;
        while started.elapsed() < CONNECT_TIMEOUT {
            tokio::time::sleep(CONNECT_POLL).await;
            let (state, joined, ip) = self.get().await.station().await;
            if let Some(state) = state
                && !seen.contains(&state)
            {
                seen.push(state);
            }
            if joined.as_deref() == Some(ssid) {
                if let Some(ip) = ip {
                    return Ok(ip);
                }
                let at = *associated_at.get_or_insert_with(tokio::time::Instant::now);
                if at.elapsed() >= DHCP_TIMEOUT {
                    return Err("dhcp_timeout");
                }
            }
        }
        let scanned = self.get().await.provisioning_scan().await.ok();
        Err(connect_failure(&seen, scanned.as_deref(), ssid))
    }

    async fn restore_wifi(&self, snapshot: Value, remove: Vec<String>) {
        self.get()
            .await
            .provisioning_restore(self.signal_emitter(), &snapshot, &remove)
            .await;
    }

    async fn claim(&self, staged: &StagedClaim) -> Result<Option<String>, &'static str> {
        self.get()
            .await
            .provisioning_claim(self.signal_emitter(), staged)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_connect_is_named_from_what_the_station_went_through() {
        let states = |states: &[&str]| states.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let scan = [
            json!({ "ssid": "Harbour", "security": "wpa2-psk" }),
            json!({ "ssid": "Marina", "security": "wpa2-eap" }),
        ];
        assert_eq!(
            connect_failure(
                &states(&["SCANNING", "4WAY_HANDSHAKE", "DISCONNECTED"]),
                Some(&scan),
                "Harbour"
            ),
            "wrong_password"
        );
        assert_eq!(
            connect_failure(&states(&["COMPLETED"]), None, "Harbour"),
            "dhcp_timeout"
        );
        assert_eq!(
            connect_failure(&states(&["SCANNING"]), Some(&scan), "Lagoon"),
            "ssid_not_found"
        );
        assert_eq!(
            connect_failure(&states(&["SCANNING"]), Some(&scan), "Marina"),
            "unsupported_security"
        );
        assert_eq!(
            connect_failure(&states(&["SCANNING"]), Some(&scan), "Harbour"),
            "connect_failed"
        );
        assert_eq!(connect_failure(&[], None, "Harbour"), "connect_failed");
    }
}
