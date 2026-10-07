//! The Wi-Fi station and access point, read through their control sockets.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::*;

/// One wireless interface's association, as wpa_supplicant reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiAssociation {
    /// The interface name.
    pub interface: String,
    /// `wpa_state`: `COMPLETED`, `SCANNING`, `DISCONNECTED`, ...
    pub state: Option<String>,
    /// The associated network's SSID.
    pub ssid: Option<String>,
    /// The associated access point.
    pub bssid: Option<String>,
    /// The channel frequency in MHz.
    pub frequency_mhz: Option<u32>,
    /// `key_mgmt`: `WPA2-PSK`, `SAE`, ...
    pub key_management: Option<String>,
    /// The received signal strength, dBm.
    pub rssi_dbm: Option<i32>,
    /// The current link speed, Mb/s.
    pub link_speed_mbps: Option<u32>,
    /// Why nothing could be observed, when nothing could.
    pub detail: Option<String>,
}

/// What the wireless observation produced.
#[derive(Debug, Clone, Default)]
pub struct WifiEvidence {
    /// Whether wpa_supplicant's control directory exists at all.
    pub control_dir_present: bool,
    /// One entry per wireless interface asked.
    pub associations: Vec<WifiAssociation>,
    /// One entry per wireless interface hostapd was asked about.
    ///
    /// Carried here rather than beside it because it is the same question
    /// asked of the other end of the same radio: what this device is
    /// associated with, and what is associated with this device.
    pub access_points: Vec<ApEvidence>,
}

/// One client associated with this device's access point, as hostapd reports
/// it.
///
/// **No credential of any kind.** hostapd's `STA <mac>` reply carries key
/// negotiation state; what is read here is who is connected and how well, and
/// nothing that could authenticate anyone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApStation {
    /// The station's hardware address, which is how hostapd names it.
    pub mac: String,
    /// `connected_time` in seconds, as hostapd counts it.
    pub connected_seconds: Option<u64>,
    /// Signal, in dBm.
    pub signal_dbm: Option<i32>,
    /// Bytes the station received from this device, and sent to it.
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
}

/// What the access point reports about the clients on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApEvidence {
    /// The interface asked.
    pub interface: String,
    /// Whether hostapd answered at all.
    pub available: bool,
    /// Why it did not, when it did not.
    pub detail: Option<String>,
    /// The stations, in the order hostapd walked them.
    pub stations: Vec<ApStation>,
}

/// Parse one `STA <mac>` reply into a station.
///
/// The reply is `key=value` lines, the first of which is the address. A field
/// this build does not read is left where it is rather than guessed at.
#[must_use]
pub fn parse_ap_station(mac: &str, text: &str) -> ApStation {
    let mut station = ApStation {
        mac: mac.to_string(),
        ..ApStation::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "connected_time" => station.connected_seconds = value.parse().ok(),
            "signal" => station.signal_dbm = value.parse().ok(),
            "rx_bytes" => station.rx_bytes = value.parse().ok(),
            "tx_bytes" => station.tx_bytes = value.parse().ok(),
            _ => {}
        }
    }
    station
}

/// The first line of a `STA-FIRST`/`STA-NEXT` reply: the station's address.
///
/// hostapd answers `FAIL` when the walk is over, and an empty reply when
/// there is nothing to walk.
#[must_use]
pub fn station_address(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    if first.is_empty() || first == "FAIL" || first == "UNKNOWN COMMAND" {
        return None;
    }
    // A MAC and nothing else: the walk's reply starts with the address, and
    // anything that is not one means the socket answered something this build
    // does not understand.
    let is_mac = first.len() == 17
        && first
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b':');
    is_mac.then(|| first.to_string())
}

/// Parse wpa_supplicant's `STATUS` reply (`key=value` lines) into an
/// association. Only the association facts are read: the interface's own
/// hardware address (`address=`) is networkd's to report, and nothing here
/// carries a credential.
#[must_use]
pub fn parse_wpa_status(interface: &str, text: &str) -> WifiAssociation {
    let mut association = WifiAssociation {
        interface: interface.to_string(),
        ..WifiAssociation::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "wpa_state" => association.state = Some(value.to_string()),
            "ssid" => association.ssid = Some(value.to_string()),
            "bssid" => association.bssid = Some(value.to_string()),
            "freq" => association.frequency_mhz = value.parse().ok(),
            "key_mgmt" => association.key_management = Some(value.to_string()),
            _ => {}
        }
    }
    if association.state.is_none() {
        association.detail = Some("wpa_supplicant answered without a wpa_state".to_string());
    }
    association
}

/// Fold wpa_supplicant's `SIGNAL_POLL` reply into `association`.
pub fn apply_signal_poll(association: &mut WifiAssociation, text: &str) {
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "RSSI" => association.rssi_dbm = value.trim().parse().ok(),
            "LINKSPEED" => association.link_speed_mbps = value.trim().parse().ok(),
            "FREQUENCY" if association.frequency_mhz.is_none() => {
                association.frequency_mhz = value.trim().parse().ok();
            }
            _ => {}
        }
    }
}

/// Parse `/proc/net/wireless` into interface → signal level (dBm).
///
/// The level column is the third numeric field after the interface name
/// (status, link, level); the kernel prints it with a trailing `.`.
#[must_use]
pub fn parse_proc_net_wireless(text: &str) -> BTreeMap<String, i32> {
    let mut levels = BTreeMap::new();
    for line in text.lines().skip(2) {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let name = name.trim_end_matches(':');
        let level = fields
            .nth(2)
            .and_then(|field| field.trim_end_matches('.').parse::<f64>().ok());
        if let Some(level) = level {
            levels.insert(name.to_string(), level as i32);
        }
    }
    levels
}

pub(super) static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Where the reply sockets are bound, beside the control directory.
///
/// The daemon answers by sending to the path this end is bound to, so the
/// path has to exist in the daemon's view of the filesystem too. The
/// temporary directory does not: micad's unit has `PrivateTmp=yes`, and a
/// reply sent to a socket in it never arrives.
const REPLY_DIR: &str = "mica";

/// The largest reply read. A datagram longer than the buffer is cut, and
/// `SCAN_RESULTS` in a busy band is longer than one page.
const MAX_REPLY: usize = 64 * 1024;

/// One request/reply exchange on wpa_supplicant's control socket for
/// `interface`, blocking, under `timeout`.
pub(super) fn wpa_query(
    control_dir: &Path,
    interface: &str,
    command: &str,
    timeout: Duration,
) -> std::io::Result<String> {
    use std::os::unix::net::UnixDatagram;
    let reply_dir = control_dir.parent().unwrap_or(control_dir).join(REPLY_DIR);
    std::fs::create_dir_all(&reply_dir)?;
    let client_path = reply_dir.join(format!(
        "micad-wpa-{}-{}",
        std::process::id(),
        CLIENT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&client_path);
    let socket = UnixDatagram::bind(&client_path)?;
    let exchange = (|| {
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        socket.connect(control_dir.join(interface))?;
        socket.send(command.as_bytes())?;
        let mut buffer = vec![0u8; MAX_REPLY];
        let received = socket.recv(&mut buffer)?;
        Ok(String::from_utf8_lossy(&buffer[..received]).into_owned())
    })();
    let _ = std::fs::remove_file(&client_path);
    exchange
}

/// One network a scan found.
///
/// What wpa_supplicant's `SCAN_RESULTS` prints, and nothing derived: the
/// console decides what to do with a hidden SSID or an open network, and a
/// reader that dropped either would be deciding for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScannedNetwork {
    pub bssid: String,
    pub frequency_mhz: Option<u32>,
    pub signal_dbm: Option<i32>,
    /// wpa_supplicant's own flag string, e.g. `[WPA2-PSK-CCMP][ESS]`.
    pub flags: String,
    /// Empty for a network that does not broadcast its name.
    pub ssid: String,
}

/// The most results one scan reports.
pub(super) const MAX_SCAN_RESULTS: usize = 64;

/// Parse `SCAN_RESULTS`: a header line, then one tab-separated row per
/// network.
#[must_use]
pub fn parse_scan_results(text: &str) -> Vec<ScannedNetwork> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            // `ssid` is last and may be empty, so the split is bounded rather
            // than required to produce five parts.
            let mut fields = line.splitn(5, '\t');
            let bssid = fields.next()?.trim();
            if bssid.is_empty() {
                return None;
            }
            Some(ScannedNetwork {
                bssid: bssid.to_string(),
                frequency_mhz: fields.next().and_then(|value| value.trim().parse().ok()),
                signal_dbm: fields.next().and_then(|value| value.trim().parse().ok()),
                flags: fields.next().unwrap_or("").trim().to_string(),
                ssid: fields.next().unwrap_or("").trim().to_string(),
            })
        })
        .take(MAX_SCAN_RESULTS)
        .collect()
}

/// Ask wpa_supplicant to scan, then read what it found.
///
/// Blocking, like every other control-socket exchange here. The scan is
/// started and the results are read after a bounded wait rather than on a
/// subscription: the console asks for a scan when an operator presses a
/// button, and a request that answered nothing because the radio was still
/// sweeping would be a button that does nothing every other press.
/// # Errors
///
/// Returns the sentence the caller publishes when wpa_supplicant is not
/// running on the interface or does not answer.
pub fn scan_networks(root: &Path, interface: &str) -> Result<Vec<ScannedNetwork>, String> {
    let control_dir = root.join(WPA_CONTROL_DIR);
    if !control_dir.join(interface).exists() {
        return Err("wpa_supplicant is not running on this interface".to_string());
    }
    // A refused `SCAN` is not fatal: `FAIL-BUSY` means a scan is already
    // running, and its results are what the read below returns.
    let _ = wpa_query(&control_dir, interface, "SCAN", WPA_TIMEOUT);
    std::thread::sleep(SCAN_SETTLE);
    let results = wpa_query(&control_dir, interface, "SCAN_RESULTS", WPA_TIMEOUT)
        .map_err(|err| format!("wpa_supplicant could not be asked: {err}"))?;
    Ok(parse_scan_results(&results))
}

/// Walk one access point's stations over hostapd's control socket.
///
/// Blocking, so it is called from a blocking task like the station query
/// beside it. Bounded by [`MAX_STATIONS`]: the walk is one round trip per
/// station and this is an observation somebody is waiting on.
#[must_use]
pub fn observe_access_point(root: &Path, interface: &str) -> ApEvidence {
    let control_dir = root.join(HOSTAPD_CONTROL_DIR);
    let mut evidence = ApEvidence {
        interface: interface.to_string(),
        ..ApEvidence::default()
    };
    if !control_dir.join(interface).exists() {
        evidence.detail =
            Some("hostapd's control socket is absent: no access point is running".to_string());
        return evidence;
    }
    let mut command = "STA-FIRST".to_string();
    for _ in 0..MAX_STATIONS {
        let Ok(reply) = wpa_query(&control_dir, interface, &command, WPA_TIMEOUT) else {
            evidence.detail = Some("hostapd could not be asked".to_string());
            return evidence;
        };
        evidence.available = true;
        let Some(mac) = station_address(&reply) else {
            break;
        };
        evidence.stations.push(parse_ap_station(&mac, &reply));
        command = format!("STA-NEXT {mac}");
    }
    evidence.available = true;
    evidence
}

/// One access point and its stations, rendered.
pub(super) fn access_point_json(evidence: &ApEvidence) -> Value {
    let stations: Vec<Value> = evidence
        .stations
        .iter()
        .map(|station| {
            let mut entry = serde_json::Map::new();
            entry.insert("mac".to_string(), json!(station.mac));
            if let Some(seconds) = station.connected_seconds {
                entry.insert("connectedSeconds".to_string(), json!(seconds));
            }
            if let Some(signal) = station.signal_dbm {
                entry.insert("signalDbm".to_string(), json!(signal));
            }
            if let Some(bytes) = station.rx_bytes {
                entry.insert("rxBytes".to_string(), json!(bytes));
            }
            if let Some(bytes) = station.tx_bytes {
                entry.insert("txBytes".to_string(), json!(bytes));
            }
            Value::Object(entry)
        })
        .collect();
    json!({
        "interface": evidence.interface,
        "stationCount": stations.len(),
        "stations": stations,
    })
}
