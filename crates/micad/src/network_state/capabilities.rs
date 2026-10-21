//! What the radio can do as an access point, from `iw`: the security types,
//! the bands, automatic channel selection, a station and an access point at
//! once, and the 2.4 GHz channels the hardware has.

use std::path::Path;

use serde_json::{Value, json};

use crate::openrc::Commands;

/// The hostapd binary whose build decides whether it can select a channel.
pub const HOSTAPD_BINARY: &str = "usr/sbin/hostapd";
/// What hostapd built without automatic channel selection says when a
/// configuration asks for it.
const NO_ACS_MARKER: &[u8] = b"CONFIG_ACS disabled";
/// The largest hostapd binary searched for [`NO_ACS_MARKER`].
const MAX_HOSTAPD_BYTES: u64 = 16 * 1024 * 1024;

/// Whether the hostapd at `binary` can select a channel itself: it exists,
/// and it was not built without ACS, which leaves [`NO_ACS_MARKER`] in it.
#[must_use]
pub fn hostapd_has_acs(binary: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(binary) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_HOSTAPD_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    !bytes
        .windows(NO_ACS_MARKER.len())
        .any(|window| window == NO_ACS_MARKER)
}

/// What `iw phy <phy> info` says about one radio.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhyInfo {
    /// `AP` is among the supported interface modes.
    pub access_point: bool,
    /// SAE can run in access-point mode: in hostapd with the AUTHENTICATE
    /// command, or offloaded to the firmware.
    pub sae: bool,
    /// The BIP-CMAC cipher protected management frames need.
    pub cmac: bool,
    /// The bands the radio has frequencies in: `2.4`, `5`, `6`, `60`.
    pub bands: Vec<&'static str>,
    /// The 2.4 GHz channel numbers the radio has, whatever the regulatory
    /// domain currently allows.
    pub channels: Vec<u8>,
    /// One valid interface combination holds a station and an access point.
    pub ap_client_concurrent: bool,
}

fn band_of(frequency_mhz: f64) -> Option<&'static str> {
    match frequency_mhz {
        f if (2400.0..2500.0).contains(&f) => Some("2.4"),
        f if (4900.0..5925.0).contains(&f) => Some("5"),
        f if (5925.0..7125.0).contains(&f) => Some("6"),
        f if (57_000.0..71_000.0).contains(&f) => Some("60"),
        _ => None,
    }
}

/// Whether one `valid interface combinations` entry holds a station and an
/// access point at once: `#{ managed } <= 1, #{ AP, P2P-GO } <= 1, total <= 2`.
fn combination_is_concurrent(entry: &str) -> bool {
    let mut groups: Vec<(Vec<&str>, u32)> = Vec::new();
    let mut total = 0;
    for part in entry.split("#{").skip(1) {
        let Some((names, rest)) = part.split_once('}') else {
            continue;
        };
        let limit = rest
            .trim_start()
            .strip_prefix("<=")
            .and_then(|rest| {
                rest.trim_start()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
            })
            .and_then(|limit| limit.parse().ok())
            .unwrap_or(0);
        groups.push((names.split(',').map(str::trim).collect(), limit));
    }
    if let Some(rest) = entry.split("total <=").nth(1) {
        total = rest
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|total| total.parse().ok())
            .unwrap_or(0);
    }
    let holds = |mode: &str| {
        groups
            .iter()
            .position(|(names, limit)| *limit >= 1 && names.contains(&mode))
    };
    match (holds("managed"), holds("AP")) {
        (Some(station), Some(ap)) if station != ap => total >= 2,
        (Some(group), Some(_)) => total >= 2 && groups[group].1 >= 2,
        _ => false,
    }
}

/// Parse `iw phy <phy> info`.
#[must_use]
pub fn parse_phy_info(text: &str) -> PhyInfo {
    let mut info = PhyInfo::default();
    let mut section = "";
    let mut combinations: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(':') && !trimmed.starts_with('*') {
            section = trimmed;
            continue;
        }
        if trimmed.contains("Device supports SAE with AUTHENTICATE command")
            || trimmed.contains("[ SAE_OFFLOAD_AP ]")
        {
            info.sae = true;
        }
        let item = trimmed.strip_prefix("* ");
        match section {
            "Supported interface modes:" => {
                if item == Some("AP") {
                    info.access_point = true;
                }
            }
            "Supported Ciphers:" => {
                if item.is_some_and(|item| item.contains("CMAC")) {
                    info.cmac = true;
                }
            }
            "valid interface combinations:" => match item {
                Some(item) => combinations.push(item.to_string()),
                None => {
                    if let Some(last) = combinations.last_mut() {
                        last.push(' ');
                        last.push_str(trimmed);
                    }
                }
            },
            _ => {}
        }
        // `* 2412 MHz [1] (20.0 dBm)`, or `* 2412.0 MHz [1] ...` on a newer iw.
        if let Some(item) = item
            && let Some((frequency, rest)) = item.split_once(" MHz [")
            && let Ok(frequency) = frequency.trim().parse::<f64>()
        {
            if let Some(band) = band_of(frequency)
                && !info.bands.contains(&band)
            {
                info.bands.push(band);
            }
            if band_of(frequency) == Some("2.4")
                && let Some(channel) = rest.split(']').next().and_then(|c| c.parse::<u8>().ok())
                && !info.channels.contains(&channel)
            {
                info.channels.push(channel);
            }
        }
    }
    info.channels.sort_unstable();
    info.ap_client_concurrent = combinations
        .iter()
        .any(|entry| combination_is_concurrent(entry));
    info
}

/// The security types an access point on this radio can offer: WPA2 and
/// open always, and the WPA2/WPA3 mix where SAE runs in access-point mode
/// and the radio has the BIP cipher protected management frames need.
#[must_use]
pub fn ap_security(info: &PhyInfo) -> Vec<&'static str> {
    let mut security = vec!["wpa2-psk"];
    if info.sae && info.cmac {
        security.push("wpa2-wpa3");
    }
    security.push("open");
    security
}

/// What the radio behind `interface` can do, as members of
/// `capabilities.wifi`: `security`, `bands`, `acs`, `apClientConcurrent` and
/// `channels`. `None` when `iw` could not say.
pub async fn wifi_capabilities(
    root: &Path,
    commands: &dyn Commands,
    interface: &str,
) -> Option<Value> {
    let phy = mica_fs::read_trimmed(
        &root
            .join(super::NET_CLASS_DIR)
            .join(interface)
            .join("phy80211/name"),
    )?;
    let info = commands.run("iw", &["phy", &phy, "info"]).await.ok()?;
    if info.code != 0 {
        return None;
    }
    let info = parse_phy_info(&info.stdout);
    if !info.access_point {
        return Some(json!({
            "security": [],
            "bands": info.bands,
            "acs": false,
            "apClientConcurrent": false,
            "channels": info.channels,
            "accessPoint": false,
        }));
    }
    // hostapd selects a channel from the survey the driver keeps.
    let survey = commands
        .run("iw", &["dev", interface, "survey", "dump"])
        .await
        .is_ok_and(|output| output.code == 0 && output.stdout.contains("frequency:"));
    Some(json!({
        "security": ap_security(&info),
        "bands": info.bands,
        "acs": survey && hostapd_has_acs(&root.join(HOSTAPD_BINARY)),
        "apClientConcurrent": info.ap_client_concurrent,
        "channels": info.channels,
        "accessPoint": true,
    }))
}
