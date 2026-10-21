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
    /// Whether the station role is started. On by default: a running
    /// supplicant with no network joins nothing, and a scan needs one.
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
            enabled: true,
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
    /// Whether the station joins this network by itself; absent is true. A
    /// network with false is kept and never joined.
    #[serde(
        rename = "autoConnect",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_connect: Option<bool>,
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

/// The regulatory domains that follow the FCC's 2.4 GHz plan.
const FCC_PLAN_COUNTRIES: [&str; 8] = ["US", "CA", "MX", "TW", "CO", "PE", "AR", "CL"];

/// The 2.4 GHz channels an access point may use in `country_code`: 1–11
/// where the regulator follows the FCC's plan, 1–13 elsewhere. Channel 14,
/// 802.11b-only and only in Japan, is not offered.
#[must_use]
pub fn ap_channels(country_code: &str) -> std::ops::RangeInclusive<u8> {
    if FCC_PLAN_COUNTRIES
        .iter()
        .any(|country| country.eq_ignore_ascii_case(country_code))
    {
        1..=11
    } else {
        1..=13
    }
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
    /// 2.4 GHz channel the access point uses; 0 lets it choose, by ACS where
    /// hostapd can and otherwise the first channel the country allows.
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
    /// How clients authenticate; absent is WPA2-PSK.
    #[serde(skip_serializing_if = "ApSecurity::is_default")]
    pub security: ApSecurity,
    /// The DHCP pool handed to clients, as an offset into the subnet of
    /// `address` and a count; absent is every address after the access
    /// point's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<ApPool>,
    /// The DHCP lease time in seconds, at least [`MIN_LEASE_SECONDS`]; absent
    /// leaves the DHCP server's own default.
    #[serde(rename = "leaseSeconds", skip_serializing_if = "Option::is_none")]
    pub lease_seconds: Option<u32>,
}

/// How clients of the access point authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ApSecurity {
    /// WPA2 with a pre-shared key.
    #[default]
    #[serde(rename = "wpa2-psk")]
    Wpa2Psk,
    /// WPA2 and WPA3 (SAE) side by side, with management frames protected
    /// when the client can.
    #[serde(rename = "wpa2-wpa3")]
    Wpa2Wpa3,
    /// No key at all: anyone in range can join.
    #[serde(rename = "open")]
    Open,
}

impl ApSecurity {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// An access point's DHCP pool: `size` addresses from `offset` into the
/// subnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApPool {
    /// The first address handed out, as an offset from the subnet address.
    pub offset: u32,
    /// How many addresses the pool holds.
    pub size: u32,
}

/// The shortest DHCP lease the access point hands out, in seconds.
pub const MIN_LEASE_SECONDS: u32 = 60;

/// Refuse an access point's pool and lease the DHCP server cannot serve: a
/// pool that is empty, reaches the subnet or broadcast address, or holds the
/// access point's own address, and a lease shorter than
/// [`MIN_LEASE_SECONDS`]. A pool is checked against `address`, so an
/// address that is not IPv4 CIDR leaves the pool unchecked here; the address
/// has its own rule.
pub fn validate_ap_pool(ap: &WifiApSettings) -> Result<(), String> {
    if let Some(seconds) = ap.lease_seconds
        && seconds < MIN_LEASE_SECONDS
    {
        return Err(format!(
            "wifi.ap.leaseSeconds {seconds} is shorter than {MIN_LEASE_SECONDS} seconds"
        ));
    }
    let Some(pool) = ap.pool else {
        return Ok(());
    };
    let Some((host, prefix)) = crate::network_rules::ipv4_cidr(&ap.address) else {
        return Ok(());
    };
    let span = 1u64 << (32 - u32::from(prefix));
    let host_index = u64::from(u32::from(host)) & (span - 1);
    let (first, end) = (
        u64::from(pool.offset),
        u64::from(pool.offset) + u64::from(pool.size),
    );
    if pool.size == 0 || first == 0 || end > span - 1 {
        return Err(format!(
            "wifi.ap.pool must hold at least one address between the subnet and broadcast addresses of {}",
            ap.address
        ));
    }
    if (first..end).contains(&host_index) {
        return Err(format!(
            "wifi.ap.pool holds the access point's own address {host}"
        ));
    }
    Ok(())
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
            security: ApSecurity::default(),
            pool: None,
            lease_seconds: None,
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
