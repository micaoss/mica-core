use super::super::systemd::mock::MockUnitControl;
use super::*;
use micad_settings::{ProvisioningSettings, WifiClientSettings, WifiSettings};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

mod apply;
mod conflicts;
mod dhcp;
mod identity;
mod openrc;
mod render;
mod safety;
mod units;

/// Golden render of [`lab_ap`] with an explicit SSID and key.
const GOLDEN_CONFIG: &str = "# Managed by micad from wifi.ap. Do not edit.\n\
    interface=wlan0\n\
    driver=nl80211\n\
    ssid=mica-lab\n\
    country_code=DE\n\
    ieee80211d=1\n\
    hw_mode=g\n\
    channel=11\n\
    auth_algs=1\n\
    ignore_broadcast_ssid=0\n\
    wmm_enabled=1\n\
    wpa=2\n\
    wpa_key_mgmt=WPA-PSK\n\
    rsn_pairwise=CCMP\n\
    wpa_passphrase=labsecret1\n\
    ctrl_interface=/run/hostapd\n";
/// Golden networkd unit for `wlan0` at the default AP address.
const GOLDEN_NETWORKD: &str = "[Match]\nName=wlan0\n\n\
    [Network]\nAddress=192.168.4.1/24\nDHCPServer=yes\n\n\
    [DHCPServer]\nPoolOffset=2\nPoolSize=253\n";
/// Device identifier used by the fixtures; 32 hex characters, as
/// `identity` generates.
const DEVICE_ID: &str = "1a2b3c4d5e6f708192a3b4c5d6e7f809";
/// Pre-shared key used by the leak test; distinctive enough that a
/// substring search for it cannot match anything else.
const SECRET_PSK: &str = "Zq7LEAKCANARY4x";

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
    state_dir: PathBuf,
}

impl Paths {
    /// Path of the networkd unit this reconciler renders for `wlan0`.
    fn networkd(&self) -> PathBuf {
        self.network_dir.join("90-wifi-ap-wlan0.network")
    }
}

/// Fixture rooted entirely inside `dir`. No directory exists yet, so a
/// test also proves the reconciler creates what it needs.
fn fixture(
    dir: &Path,
    active: &str,
    file_state: &str,
) -> (WifiApReconciler<MockUnitControl, MockReload>, Paths) {
    let paths = Paths {
        config: dir.join("hostapd").join("wlan0.conf"),
        config_dir: dir.join("hostapd"),
        network_dir: dir.join("network"),
        state_dir: dir.join("state"),
    };
    let reconciler = WifiApReconciler::new(
        paths.config_dir.clone(),
        paths.network_dir.clone(),
        paths.state_dir.clone(),
        MockUnitControl::new(active, file_state),
        MockReload::new(),
    );
    (reconciler, paths)
}

/// Put `psk` where [`identity::read_ap_psk`] looks for it.
fn seed_ap_psk(state_dir: &Path, psk: &str) {
    let secrets = state_dir.join("secrets");
    std::fs::create_dir_all(&secrets).expect("create secrets dir");
    std::fs::write(secrets.join("ap-psk"), format!("{psk}\n")).expect("write ap-psk");
}

/// The access point the golden render describes: explicit SSID, explicit
/// key, a non-default channel and a non-default regulatory domain so the
/// golden file cannot pass by matching the struct defaults.
fn lab_ap() -> WifiApSettings {
    WifiApSettings {
        mode: ApMode::Always,
        ssid: Some("mica-lab".to_string()),
        psk: Some("labsecret1".to_string()),
        channel: 11,
        country_code: "DE".to_string(),
        ..WifiApSettings::default()
    }
}

/// Settings carrying `ap`, a provisioned device identity, and a station
/// role that is switched off.
fn settings_with(ap: WifiApSettings) -> Settings {
    Settings {
        provisioning: ProvisioningSettings {
            device_id: Some(DEVICE_ID.to_string()),
            ..ProvisioningSettings::default()
        },
        wifi: WifiSettings {
            ap,
            ..WifiSettings::default()
        },
        ..Settings::default()
    }
}

/// `settings_with`, plus a station role on `interface`.
fn settings_with_station(ap: WifiApSettings, interface: &str) -> Settings {
    let mut settings = settings_with(ap);
    settings.wifi.client = WifiClientSettings {
        enabled: true,
        interface: interface.to_string(),
        networks: Vec::new(),
    };
    settings
}

fn mode_of(path: &Path) -> u32 {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.permissions().mode() & 0o7777,
        Err(err) => panic!("stat {}: {err}", path.display()),
    }
}

// ---- rendering --------------------------------------------------------
