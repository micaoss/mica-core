//! BLE provisioning's device side: what a session reads and changes, through
//! the same settings writes and observers the bus members use.

use std::path::Path;
use std::time::Duration;

use micad_settings::{ApMode, Feature, IfaceKind, IfaceSettings, StaticConfig, WifiNetwork};
use serde_json::{Value, json};
use zbus::object_server::{InterfaceRef, SignalEmitter};

use super::*;
use crate::ble_provisioning::engine::{Device, Facts};

/// The sender a provisioning write is recorded as.
const SENDER: &str = "ble-provisioning";
/// How long a `setWifi` waits for an address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long it waits for an address once associated.
const DHCP_TIMEOUT: Duration = Duration::from_secs(30);
/// How often it looks.
const CONNECT_POLL: Duration = Duration::from_millis(500);
/// How old a scan a session answers with.
const SCAN_FRESH: Duration = Duration::from_secs(30);
/// How long a `setNetwork` waits for its address.
const WIRED_TIMEOUT: Duration = Duration::from_secs(30);
/// How often it looks.
const NETWORK_POLL: Duration = Duration::from_millis(500);

/// `wpa_state`s that mean the key was being negotiated.
const HANDSHAKE_STATES: [&str; 2] = ["4WAY_HANDSHAKE", "GROUP_HANDSHAKE"];

impl MicadService {
    /// Persist `writes` and queue their apply, as `SetSettingsMany` does.
    async fn provisioning_write(
        &self,
        emitter: &SignalEmitter<'_>,
        writes: &[(String, Value)],
    ) -> Result<(), micad_settings::SettingsError> {
        self.persist_settings(writes).await?;
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

    /// Each interface's IPv4 address with its prefix and, from networkd, where
    /// it came from; from the kernel over rtnetlink where networkd does not
    /// answer (OpenRC), with no source. None when neither answers.
    async fn ipv4_addresses(
        &self,
    ) -> Option<std::collections::BTreeMap<String, (String, Option<String>)>> {
        if let Some(observed) = self.networkd_ipv4_addresses().await {
            return Some(observed);
        }
        let kernel = tokio::task::spawn_blocking(crate::netlink::ipv4_addresses)
            .await
            .ok()?
            .ok()?;
        Some(
            kernel
                .into_iter()
                .map(|(name, address)| (name, (address, None)))
                .collect(),
        )
    }

    async fn networkd_ipv4_addresses(
        &self,
    ) -> Option<std::collections::BTreeMap<String, (String, Option<String>)>> {
        let observed = self.network_state.observe().await.ok()?;
        let entries = observed.pointer("/interfaces/entries")?.as_array()?;
        Some(
            entries
                .iter()
                .filter_map(|entry| {
                    let address = entry["addresses"]
                        .as_array()?
                        .iter()
                        .find(|address| address["family"] == "ipv4")?;
                    Some((
                        entry["name"].as_str()?.to_string(),
                        (
                            format!(
                                "{}/{}",
                                address["address"].as_str()?,
                                address["prefixLength"].as_u64()?
                            ),
                            address["configSource"].as_str().map(str::to_string),
                        ),
                    ))
                })
                .collect(),
        )
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
        let addresses = self.ipv4_addresses().await.unwrap_or_default();
        let interfaces: Vec<Value> = physical_interfaces(Path::new("/"))
            .into_iter()
            .map(|link| {
                json!({
                    "name": link.name,
                    "kind": link.kind,
                    "carrier": link.carrier,
                    "address": addresses.get(&link.name).map(|(address, _)| address),
                })
            })
            .collect();
        json!({
            "wifiMode": mode,
            "connected": ssid.is_some() && ip.is_some(),
            "ssid": ssid,
            "ip": ip,
            "interfaces": interfaces,
        })
    }

    /// `setNetwork`: `entry` as `interface`'s addressing, held to the rules
    /// apid's network routes hold a write to, whose codes are its reasons;
    /// the entry it replaced, or null.
    async fn provisioning_set_network(
        &self,
        emitter: &SignalEmitter<'_>,
        interface: &str,
        entry: &Value,
    ) -> Result<Value, &'static str> {
        if !physical_interfaces(Path::new("/"))
            .iter()
            .any(|link| link.name == interface && link.kind == "ethernet")
        {
            return Err("unknown_interface");
        }
        let mut network = self.inner.read().await.settings.network.clone();
        let previous = network.get(interface).cloned();
        let mut next = previous.clone().unwrap_or_default();
        if next.kind != IfaceKind::Physical {
            return Err("unknown_interface");
        }
        next.dns.clear();
        match wired_static(entry)? {
            None => {
                next.dhcp = true;
                next.static_ = None;
            }
            Some(addressing) => {
                next.dhcp = false;
                next.static_ = Some(addressing);
            }
        }
        network.insert(interface.to_string(), next);
        if let Some(violation) = micad_settings::map_violations(&network, &[interface]).first() {
            tracing::warn!(interface, message = %violation.message, "a provisioned wired entry was refused");
            return Err(violation.code);
        }
        let written = serde_json::to_value(&network).map_err(|_| "validation_failed")?;
        self.provisioning_write(emitter, &[("network".to_string(), written)])
            .await
            .map_err(|err| {
                tracing::warn!(error = %err, interface, "the provisioned wired entry could not be written");
                "validation_failed"
            })?;
        Ok(previous.map_or(Value::Null, |previous| {
            serde_json::to_value(previous).unwrap_or(Value::Null)
        }))
    }

    /// Put back the wired entries a session replaced; null removes one.
    async fn provisioning_restore_network(
        &self,
        emitter: &SignalEmitter<'_>,
        previous: &[(String, Value)],
    ) {
        let mut network = self.inner.read().await.settings.network.clone();
        for (interface, entry) in previous {
            match serde_json::from_value::<IfaceSettings>(entry.clone()) {
                Ok(entry) => {
                    network.insert(interface.clone(), entry);
                }
                Err(_) => {
                    network.remove(interface);
                }
            }
        }
        let Ok(written) = serde_json::to_value(&network) else {
            return;
        };
        if let Err(err) = self
            .provisioning_write(emitter, &[("network".to_string(), written)])
            .await
        {
            tracing::warn!(error = %err, "the wired entries a provisioning session changed could not be restored");
        }
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
        self.provisioning_write(emitter, &writes)
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
        if let Err(err) = self.provisioning_write(emitter, &writes).await {
            tracing::warn!(error = %err, "the Wi-Fi a provisioning session changed could not be restored");
        }
    }

    /// Refuse a bus write that reaches the Wi-Fi settings while a BLE
    /// provisioning session holds the radio. The session's own writes do
    /// not come through here.
    pub(super) fn refuse_while_wifi_locked(&self, paths: &[String]) -> Result<(), SettingsFault> {
        let reaches_wifi = paths.iter().any(|path| {
            micad_settings::path_segments(path)
                .is_none_or(|segments| segments.first().is_none_or(|first| first == "wifi"))
        });
        if reaches_wifi && self.provisioning.wifi_locked() {
            return Err(SettingsFault::WifiLocked(
                "a Bluetooth provisioning session is changing Wi-Fi; try again when it ends"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Re-run the Bluetooth reconciler: the radio was taken or given back.
    pub(crate) async fn apply_bluetooth(&self) {
        let _apply = self.apply_lock.lock().await;
        self.apply_subtree("bluetooth").await;
    }
}

/// A physical interface, as `getStatus` lists it.
struct Link {
    name: String,
    /// `ethernet` or `wifi`.
    kind: &'static str,
    carrier: bool,
}

/// The physical Ethernet and Wi-Fi interfaces under `root`'s
/// `/sys/class/net`, by name.
fn physical_interfaces(root: &Path) -> Vec<Link> {
    let net = root.join("sys/class/net");
    let mut links: Vec<Link> = std::fs::read_dir(&net)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let read = |name: &str| std::fs::read_to_string(path.join(name)).unwrap_or_default();
            // ARPHRD_ETHER, on a device of its own: no bridge, VLAN or tunnel.
            if read("type").trim() != "1" || !path.join("device").exists() {
                return None;
            }
            Some(Link {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: if path.join("wireless").exists() || path.join("phy80211").exists() {
                    "wifi"
                } else {
                    "ethernet"
                },
                carrier: read("carrier").trim() == "1",
            })
        })
        .collect();
    links.sort_by(|a, b| a.name.cmp(&b.name));
    links
}

/// `setNetwork`'s `static`, checked: none for `{"dhcp":true}`.
fn wired_static(entry: &Value) -> Result<Option<StaticConfig>, &'static str> {
    let Some(addressing) = entry.get("static") else {
        return Ok(None);
    };
    let address = addressing["address"]
        .as_str()
        .filter(|address| micad_settings::valid_cidr(address))
        .ok_or("invalid_address")?
        .to_string();
    let gateway = match &addressing["gateway"] {
        Value::Null => None,
        value => Some(
            value
                .as_str()
                .filter(|gateway| micad_settings::valid_ipv4(gateway))
                .ok_or("invalid_gateway")?
                .to_string(),
        ),
    };
    let dns = match &addressing["dns"] {
        Value::Null => Vec::new(),
        Value::Array(servers) => servers
            .iter()
            .map(|server| {
                server
                    .as_str()
                    .filter(|server| server.parse::<std::net::IpAddr>().is_ok())
                    .map(str::to_string)
                    .ok_or("invalid_dns")
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("invalid_dns"),
    };
    Ok(Some(StaticConfig {
        address,
        gateway,
        dns,
    }))
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
        let service = self.get().await;
        service
            .provisioning
            .scanned(SCAN_FRESH, service.provisioning_scan())
            .await
    }

    async fn warm(&self) {
        let _ = self.scan().await;
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

    async fn set_network(&self, interface: &str, entry: &Value) -> Result<Value, &'static str> {
        self.get()
            .await
            .provisioning_set_network(self.signal_emitter(), interface, entry)
            .await
    }

    async fn await_network(
        &self,
        interface: &str,
        entry: &Value,
    ) -> Result<Option<String>, &'static str> {
        let wanted = entry["static"]["address"].as_str().map(str::to_string);
        let started = tokio::time::Instant::now();
        loop {
            let carrier = physical_interfaces(Path::new("/"))
                .iter()
                .any(|link| link.name == interface && link.carrier);
            if !carrier {
                return Err("no_carrier");
            }
            // Without a reading of the addresses there is nothing to wait on.
            let Some(addresses) = self.get().await.ipv4_addresses().await else {
                return Ok(None);
            };
            let found = addresses
                .get(interface)
                .filter(|(address, source)| match &wanted {
                    Some(wanted) => address == wanted,
                    // The kernel does not say where an address came from.
                    None => source.as_deref().is_none_or(|source| source == "DHCPv4"),
                });
            if let Some((address, _)) = found {
                return Ok(Some(address.clone()));
            }
            if started.elapsed() >= WIRED_TIMEOUT {
                return match wanted {
                    None => Err("dhcp_timeout"),
                    Some(_) => Ok(None),
                };
            }
            tokio::time::sleep(NETWORK_POLL).await;
        }
    }

    async fn restore_network(&self, previous: Vec<(String, Value)>) {
        self.get()
            .await
            .provisioning_restore_network(self.signal_emitter(), &previous)
            .await;
    }

    async fn restore_wifi(&self, snapshot: Value, remove: Vec<String>) {
        self.get()
            .await
            .provisioning_restore(self.signal_emitter(), &snapshot, &remove)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_physical_interfaces_are_the_ethernet_and_wi_fi_ports() {
        let root = tempfile::tempdir().unwrap();
        let link = |name: &str, kind: &str, device: bool, files: &[(&str, &str)]| {
            let dir = root.path().join("sys/class/net").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
            if device {
                std::fs::create_dir(dir.join("device")).unwrap();
            }
            for (file, text) in files {
                if text.is_empty() {
                    std::fs::create_dir(dir.join(file)).unwrap();
                } else {
                    std::fs::write(dir.join(file), text).unwrap();
                }
            }
        };
        link("eth0", "1", true, &[("carrier", "1\n")]);
        link("eth1", "1", true, &[]);
        link("wlan0", "1", true, &[("wireless", ""), ("carrier", "0\n")]);
        link("br0", "1", false, &[("carrier", "1\n")]);
        link("can0", "280", true, &[]);
        link("lo", "772", false, &[]);
        let links: Vec<_> = physical_interfaces(root.path())
            .into_iter()
            .map(|link| (link.name, link.kind, link.carrier))
            .collect();
        assert_eq!(
            links,
            [
                ("eth0".to_string(), "ethernet", true),
                ("eth1".to_string(), "ethernet", false),
                ("wlan0".to_string(), "wifi", false),
            ]
        );
    }

    #[test]
    fn a_wired_static_entry_is_an_ipv4_address_with_its_prefix() {
        assert_eq!(wired_static(&json!({ "dhcp": true })), Ok(None));
        assert_eq!(
            wired_static(&json!({ "static": {
                "address": "10.0.0.5/24", "gateway": "10.0.0.1", "dns": ["10.0.0.1", "2001:db8::1"],
            } })),
            Ok(Some(StaticConfig {
                address: "10.0.0.5/24".into(),
                gateway: Some("10.0.0.1".into()),
                dns: vec!["10.0.0.1".into(), "2001:db8::1".into()],
            }))
        );
        for (refused, reason) in [
            (json!({ "address": "10.0.0.5" }), "invalid_address"),
            (json!({ "address": "10.0.0.5/33" }), "invalid_address"),
            (json!({ "address": "fd00::5/64" }), "invalid_address"),
            (json!({}), "invalid_address"),
            (
                json!({ "address": "10.0.0.5/24", "gateway": "router" }),
                "invalid_gateway",
            ),
            (
                json!({ "address": "10.0.0.5/24", "dns": "10.0.0.1" }),
                "invalid_dns",
            ),
            (
                json!({ "address": "10.0.0.5/24", "dns": ["dns.example"] }),
                "invalid_dns",
            ),
        ] {
            assert_eq!(
                wired_static(&json!({ "static": refused })),
                Err(reason),
                "{refused}"
            );
        }
    }

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
