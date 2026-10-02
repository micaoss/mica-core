//! The Wi-Fi client and access point settings.

/// WiFi settings, reconciled by connd into wpa_supplicant and hostapd.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiSettings {
    /// Station (uplink) configuration.
    pub client: WifiClientSettings,
    /// Access-point (provisioning) configuration.
    pub ap: WifiApSettings,
}

/// WiFi station configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiClientSettings {
    /// Whether the station role is started.
    pub enabled: bool,
    /// Interface the station role runs on.
    pub interface: String,
    /// Known networks, most preferred by `priority`.
    ///
    /// Written as a whole JSON array through the dot-path API; the path syntax
    /// has no array indexing.
    pub networks: Vec<WifiNetwork>,
}

impl Default for WifiClientSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: "wlan0".to_string(),
            networks: Vec::new(),
        }
    }
}

/// One known WiFi network.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiNetwork {
    /// Network name.
    pub ssid: String,
    /// Pre-shared key; absent means an open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    /// Whether the network hides its SSID.
    #[serde(default)]
    pub hidden: bool,
    /// Selection preference; higher wins.
    #[serde(default)]
    pub priority: i32,
}

/// Characters a raw 256-bit pre-shared key occupies, spelled in hex.
pub const RAW_PMK_LEN: usize = 64;

/// IEEE 802.11i's shortest WPA2 passphrase.
pub const MIN_PASSPHRASE_LEN: usize = 8;

/// IEEE 802.11i's longest.
pub const MAX_PASSPHRASE_LEN: usize = 63;

/// True when `value` can be carried inside a wpa_supplicant double-quoted
/// string with no way of ending the string early.
#[must_use]
pub fn is_wpa_quotable(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'"' && byte != b'\\')
}

/// Refuse a [`WifiNetwork::psk`] no WPA2 supplicant could use.
///
/// The bound is IEEE 802.11i's and it is checked for the reason the access
/// point checks it: wpa_supplicant rejects an out-of-range passphrase by
/// refusing the WHOLE configuration file, which takes every other configured
/// network down with it while the reconcile still reports `applied`.
pub fn validate_wifi_psk(psk: &str) -> Result<(), String> {
    if psk.len() == RAW_PMK_LEN && psk.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    if psk.len() < MIN_PASSPHRASE_LEN || psk.len() > MAX_PASSPHRASE_LEN {
        return Err(format!(
            "a WPA2 passphrase is {MIN_PASSPHRASE_LEN} to {MAX_PASSPHRASE_LEN} characters \
             (or a {RAW_PMK_LEN}-digit hex PMK)"
        ));
    }
    // A passphrase has no hex form -- bare hex means a raw PMK, not a
    // passphrase -- so the renderer has nothing to fall back to and refuses.
    // Refusing here instead means a key the renderer cannot carry is never
    // accepted, rather than stored and dead. The sentence names neither the
    // value nor its length, for the reason the length bound's does not.
    if !is_wpa_quotable(psk) {
        return Err(
            "the pre-shared key contains a character wpa_supplicant configuration \
             cannot carry; use printable ASCII without a quote or a backslash"
                .to_string(),
        );
    }
    Ok(())
}

/// WiFi access-point configuration used by the provisioning flow.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiApSettings {
    /// When the access point runs.
    pub mode: ApMode,
    /// Interface the access point runs on.
    pub interface: String,
    /// Advertised SSID; absent means derive it from the device identity at
    /// render time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    /// Pre-shared key; absent means derive it from the device credential at
    /// render time. A fleet-wide constant default is forbidden
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    /// 2.4 GHz channel the access point uses.
    pub channel: u8,
    /// Regulatory domain the radio is configured for.
    #[serde(rename = "countryCode")]
    pub country_code: String,
    /// AP-side address in CIDR notation.
    pub address: String,
    /// Seconds without a usable uplink before the access point starts.
    #[serde(rename = "holdDownSeconds")]
    pub hold_down_seconds: u32,
    /// Seconds the access point stays up after an uplink is restored.
    #[serde(rename = "graceSeconds")]
    pub grace_seconds: u32,
}

impl Default for WifiApSettings {
    fn default() -> Self {
        Self {
            mode: ApMode::Off,
            interface: "wlan0".to_string(),
            ssid: None,
            psk: None,
            channel: 6,
            country_code: "US".to_string(),
            address: "192.168.4.1/24".to_string(),
            hold_down_seconds: 120,
            grace_seconds: 60,
        }
    }
}

/// When the WiFi access point runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApMode {
    /// Never.
    #[default]
    Off,
    /// Only while no usable uplink exists.
    Provisioning,
    /// Always, regardless of the uplink.
    Always,
}
