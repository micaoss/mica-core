//! Which radios the device has.

use std::path::Path;

use super::*;

/// The radios and modems sysfs shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RadioEvidence {
    /// Interfaces backed by a `phy80211` device.
    pub wifi_interfaces: Vec<String>,
    /// Adapters under `/sys/class/bluetooth`.
    pub bluetooth_adapters: Vec<String>,
    /// Interfaces the kernel types `wwan`.
    pub wwan_interfaces: Vec<String>,
}

pub(super) fn sorted_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Discover the radios under `root`.
#[must_use]
pub fn radio_evidence(root: &Path) -> RadioEvidence {
    let net = root.join(NET_CLASS_DIR);
    let mut evidence = RadioEvidence::default();
    for name in sorted_entries(&net) {
        let iface = net.join(&name);
        if iface.join("phy80211").exists() || iface.join("wireless").exists() {
            evidence.wifi_interfaces.push(name.clone());
        }
        let is_wwan = name.starts_with("wwan")
            || std::fs::read_to_string(iface.join("uevent"))
                .is_ok_and(|uevent| uevent.lines().any(|line| line == "DEVTYPE=wwan"));
        if is_wwan {
            evidence.wwan_interfaces.push(name);
        }
    }
    evidence.bluetooth_adapters = sorted_entries(&root.join(BLUETOOTH_CLASS_DIR));
    evidence
}
