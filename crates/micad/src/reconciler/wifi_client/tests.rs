use super::super::systemd::mock::MockUnitControl;
use super::*;
use micad_settings::WifiSettings;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

mod apply;
mod openrc;
mod render;
mod safety;
mod units;

/// Golden render of [`multi_network_client`].
const GOLDEN_MULTI: &str = "# Managed by micad from wifi.client. Do not edit.\n\
    ctrl_interface=/run/wpa_supplicant\n\
    update_config=0\n\
    sae_pwe=2\n\
    \n\
    network={\n\
    \tssid=\"hidden-lab\"\n\
    \tscan_ssid=1\n\
    \tpriority=20\n\
    \tkey_mgmt=WPA-PSK SAE\n\
    \tieee80211w=1\n\
    \tpsk=\"labsecret1\"\n\
    }\n\
    \n\
    network={\n\
    \tssid=\"office\"\n\
    \tpriority=10\n\
    \tkey_mgmt=WPA-PSK SAE\n\
    \tieee80211w=1\n\
    \tpsk=\"officepass\"\n\
    }\n\
    \n\
    network={\n\
    \tssid=\"guest-wifi\"\n\
    \tpriority=5\n\
    \tkey_mgmt=NONE\n\
    }\n";
/// Golden render of a station with no networks configured.
const GOLDEN_EMPTY: &str = "# Managed by micad from wifi.client. Do not edit.\n\
    ctrl_interface=/run/wpa_supplicant\n\
    update_config=0\n\
    sae_pwe=2\n";
/// Golden networkd unit for `wlan0`.
const GOLDEN_NETWORKD: &str = "[Match]\nName=wlan0\n\n[Network]\nDHCP=yes\n";
/// Pre-shared key used by the leak test; distinctive enough that a
/// substring search for it cannot match anything else.
const SECRET_PSK: &str = "Zq7-LEAKCANARY-4x";

/// Recording [`NetworkReload`] so a test can tell a needed reload from a
/// reflexive one.
struct MockReload {
    calls: Mutex<Vec<String>>,
}

impl MockReload {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        match self.calls.lock() {
            Ok(calls) => calls.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[async_trait::async_trait]
impl NetworkReload for MockReload {
    async fn reload(&self) -> Result<()> {
        match self.calls.lock() {
            Ok(mut calls) => calls.push("reload".to_string()),
            Err(poisoned) => poisoned.into_inner().push("reload".to_string()),
        }
        Ok(())
    }
}

/// Paths of a fixture, all of them under the tempdir.
struct Paths {
    config: PathBuf,
    config_dir: PathBuf,
    network_dir: PathBuf,
}

impl Paths {
    /// Path of the networkd unit this reconciler renders for `wlan0`.
    fn networkd(&self) -> PathBuf {
        self.network_dir.join("90-wifi-client-wlan0.network")
    }
}

/// Fixture rooted entirely inside `dir`. Neither directory exists yet, so
/// a test also proves the reconciler creates what it needs.
fn fixture(
    dir: &Path,
    active: &str,
    file_state: &str,
) -> (WifiClientReconciler<MockUnitControl, MockReload>, Paths) {
    let paths = Paths {
        config: dir.join("wpa_supplicant").join("wpa_supplicant-wlan0.conf"),
        config_dir: dir.join("wpa_supplicant"),
        network_dir: dir.join("network"),
    };
    let reconciler = WifiClientReconciler::new(
        paths.config_dir.clone(),
        paths.network_dir.clone(),
        MockUnitControl::new(active, file_state),
        MockReload::new(),
    );
    (reconciler, paths)
}

fn network(ssid: &str, psk: Option<&str>, hidden: bool, priority: i32) -> WifiNetwork {
    WifiNetwork {
        ssid: ssid.to_string(),
        psk: psk.map(str::to_string),
        hidden,
        priority,
    }
}

/// Mixed open and PSK, mixed hidden, differing priorities, deliberately
/// **not** in priority order so the render has to sort.
fn multi_network_client() -> WifiClientSettings {
    WifiClientSettings {
        enabled: true,
        interface: "wlan0".to_string(),
        networks: vec![
            network("office", Some("officepass"), false, 10),
            network("guest-wifi", None, false, 5),
            network("hidden-lab", Some("labsecret1"), true, 20),
        ],
    }
}

fn client(enabled: bool, networks: Vec<WifiNetwork>) -> WifiClientSettings {
    WifiClientSettings {
        enabled,
        interface: "wlan0".to_string(),
        networks,
    }
}

fn settings_with(client: WifiClientSettings) -> Settings {
    Settings {
        wifi: WifiSettings {
            client,
            ..WifiSettings::default()
        },
        ..Settings::default()
    }
}

/// The common single-network enabled case.
fn one_psk_network() -> WifiClientSettings {
    client(true, vec![network("office", Some("officepass"), false, 10)])
}

fn mode_of(path: &Path) -> u32 {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.permissions().mode() & 0o7777,
        Err(err) => panic!("stat {}: {err}", path.display()),
    }
}

// ---- rendering --------------------------------------------------------
