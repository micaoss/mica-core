//! The hostapd configuration and networkd unit the access point renders to.

use anyhow::{Result, anyhow};
use micad_settings::{MAX_PASSPHRASE_LEN, MIN_PASSPHRASE_LEN, RAW_PMK_LEN, WifiApSettings};
use std::net::Ipv4Addr;

use super::*;

/// Check that `interface` is a name the kernel could actually carry.
pub(super) fn validate_interface(interface: &str) -> Result<()> {
    micad_settings::check_iface_name("wifi.ap.interface", interface).map_err(anyhow::Error::msg)
}

/// True when `value` can be carried verbatim on the right-hand side of a
/// hostapd configuration line with no way of meaning anything else.
pub(super) fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && micad_settings::is_wpa_quotable(value)
        && !value.starts_with(' ')
        && !value.ends_with(' ')
}

/// Encode `ssid` as a hostapd SSID directive, including the key.
///
/// A plain SSID is emitted as `ssid=<value>`, which is what an operator reading
/// the file on device expects. Anything else is emitted as `ssid2=<hex>`:
/// hostapd's `ssid2` accepts a hexdump of the SSID bytes, whose alphabet is
/// `0-9a-f`, so injection is not merely escaped there — it is not expressible.
pub(super) fn ssid_directive(ssid: &str) -> String {
    if is_plain(ssid) {
        format!("ssid={ssid}")
    } else {
        format!("ssid2={}", hex::encode(ssid.as_bytes()))
    }
}

/// Check that `ssid` is a name IEEE 802.11 can carry.
///
/// # Errors
pub(super) fn validate_ssid(ssid: &str) -> Result<()> {
    if ssid.is_empty() {
        return Err(anyhow!("wifi.ap.ssid is empty"));
    }
    if ssid.len() > MAX_SSID_BYTES {
        return Err(anyhow!(
            "wifi.ap.ssid is {} bytes, more than the {MAX_SSID_BYTES} IEEE 802.11 allows",
            ssid.len()
        ));
    }
    Ok(())
}

/// Check that `country_code` is a regulatory domain hostapd can be given.
///
/// The value is rendered raw into the configuration file, so this is an
/// injection guard as much as a validity check: two ASCII letters cannot carry
/// a newline and therefore cannot start a directive of their own.
pub(super) fn validate_country_code(country_code: &str) -> Result<()> {
    if country_code.len() == 2 && country_code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Ok(());
    }
    Err(anyhow!(
        "wifi.ap.countryCode {country_code:?} is not a two-letter regulatory domain"
    ))
}

/// Encode `psk` as a hostapd key directive, including the key name.
///
/// A 64-character hex string is a raw 256-bit PMK and is emitted as
/// `wpa_psk=`; anything else is a passphrase and is emitted as
/// `wpa_passphrase=`. Using the wrong key name makes hostapd reject the file.
pub(super) fn psk_directive(psk: &str) -> Result<String> {
    if psk.len() == RAW_PMK_LEN && psk.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(format!("wpa_psk={psk}"));
    }
    if !(MIN_PASSPHRASE_LEN..=MAX_PASSPHRASE_LEN).contains(&psk.len()) {
        return Err(anyhow!(
            "the pre-shared key must be a WPA2 passphrase of {MIN_PASSPHRASE_LEN} to \
             {MAX_PASSPHRASE_LEN} characters or a raw {RAW_PMK_LEN}-digit hex PMK"
        ));
    }
    if !is_plain(psk) {
        return Err(anyhow!(
            "the pre-shared key contains a character hostapd configuration cannot \
             carry unambiguously; use printable ASCII without a quote, a backslash, \
             or a leading or trailing space"
        ));
    }
    Ok(format!("wpa_passphrase={psk}"))
}

/// Split `address` into its host address and prefix length.
///
/// # Errors
///
/// Returns an error when the value is not `A.B.C.D/prefix`, or when the prefix
/// is outside the range that can hold a host address and a pool.
pub(super) fn parse_cidr(address: &str) -> Result<(Ipv4Addr, u32)> {
    let (host, prefix) = address
        .split_once('/')
        .ok_or_else(|| anyhow!("wifi.ap.address {address:?} is not in CIDR notation"))?;
    let host: Ipv4Addr = host
        .parse()
        .map_err(|_| anyhow!("wifi.ap.address {address:?} does not start with an IPv4 address"))?;
    let prefix: u32 = prefix
        .parse()
        .map_err(|_| anyhow!("wifi.ap.address {address:?} has no numeric prefix length"))?;
    if !(MIN_PREFIX_LEN..=MAX_PREFIX_LEN).contains(&prefix) {
        return Err(anyhow!(
            "wifi.ap.address {address:?} has a /{prefix} prefix, outside \
             /{MIN_PREFIX_LEN}..=/{MAX_PREFIX_LEN}"
        ));
    }
    Ok((host, prefix))
}

/// The DHCP pool for an access point sitting at `host` in a `/prefix` subnet,
/// as networkd's `PoolOffset=` (from the subnet address) and `PoolSize=`.
pub(super) fn dhcp_pool(host: Ipv4Addr, prefix: u32) -> Result<(u32, u32)> {
    let host_bits = u32::from(host);
    let mask = u32::MAX << (32 - prefix);
    let index = host_bits - (host_bits & mask);
    let broadcast_index = (1u32 << (32 - prefix)) - 1;
    if index == 0 {
        return Err(anyhow!(
            "wifi.ap.address {host} is the subnet address of its own subnet"
        ));
    }
    if index >= broadcast_index {
        return Err(anyhow!(
            "wifi.ap.address {host} is the broadcast address of its own subnet"
        ));
    }
    let offset = index + 1;
    if offset >= broadcast_index {
        return Err(anyhow!(
            "wifi.ap.address {host}/{prefix} leaves no address for a DHCP pool"
        ));
    }
    Ok((offset, broadcast_index - offset))
}

/// Render the hostapd configuration for `ap` with the resolved `ssid` and
/// `psk`.
pub(super) fn render_config(ap: &WifiApSettings, ssid: &str, psk: &str) -> Result<String> {
    validate_ssid(ssid)?;
    validate_country_code(&ap.country_code)?;
    if ap.channel == 0 || ap.channel > MAX_CHANNEL {
        return Err(anyhow!(
            "wifi.ap.channel {} is not a 2.4 GHz channel (1..={MAX_CHANNEL})",
            ap.channel
        ));
    }
    let key = psk_directive(psk)?;

    let mut out = String::from(CONFIG_HEADER);
    out.push_str(&format!("interface={}\n", ap.interface));
    out.push_str("driver=nl80211\n");
    out.push_str(&format!("{}\n", ssid_directive(ssid)));
    out.push_str(&format!("country_code={}\n", ap.country_code));
    out.push_str("ieee80211d=1\n");
    out.push_str("hw_mode=g\n");
    out.push_str(&format!("channel={}\n", ap.channel));
    out.push_str("auth_algs=1\n");
    out.push_str("ignore_broadcast_ssid=0\n");
    out.push_str("wmm_enabled=1\n");
    out.push_str("wpa=2\n");
    out.push_str("wpa_key_mgmt=WPA-PSK\n");
    out.push_str("rsn_pairwise=CCMP\n");
    out.push_str(&format!("{key}\n"));
    out.push_str(&format!("ctrl_interface={CONTROL_DIR}\n"));
    Ok(out)
}

/// Render the networkd unit that addresses the access-point link and runs the
/// DHCP server on it.
///
/// The address is re-rendered from the parsed value rather than echoed, so a
/// value that parsed cannot carry anything else into the file.
pub(super) fn render_networkd(interface: &str, address: &str) -> Result<String> {
    let (host, prefix) = parse_cidr(address)?;
    let (offset, size) = dhcp_pool(host, prefix)?;
    Ok(format!(
        "[Match]\nName={interface}\n\n\
         [Network]\nAddress={host}/{prefix}\nDHCPServer=yes\n\n\
         [DHCPServer]\nPoolOffset={offset}\nPoolSize={size}\n"
    ))
}

/// Render busybox udhcpd's configuration for the access-point link, the
/// OpenRC counterpart of [`render_networkd`]: the same pool, the access point
/// as the clients' router.
pub(super) fn render_udhcpd(interface: &str, address: &str, pidfile: &str) -> Result<String> {
    let (host, prefix) = parse_cidr(address)?;
    let (offset, size) = dhcp_pool(host, prefix)?;
    let mask = u32::MAX << (32 - prefix);
    let subnet = u32::from(host) & mask;
    let start = Ipv4Addr::from(subnet + offset);
    let end = Ipv4Addr::from(subnet + offset + size - 1);
    Ok(format!(
        "# Managed by micad from wifi.ap. Do not edit.\n\
         interface {interface}\nstart {start}\nend {end}\n\
         option subnet {}\noption router {host}\n\
         pidfile {pidfile}\nlease_file /run/mica/wifi-ap-udhcpd.leases\n",
        Ipv4Addr::from(mask)
    ))
}

/// SSID for a device with this identifier.
///
/// Derived from the device identity and nothing else — never from a secret —
/// so the same device always advertises the same name and two devices do not
/// collide. The prefix and the identifier length match the derived hostname's,
/// so an operator seeing `mica-1a2b3c4d` on the air knows which device it is.
pub(super) fn derived_ssid(device_id: &str) -> String {
    let suffix: String = device_id.chars().take(SSID_ID_CHARS).collect();
    format!("{SSID_PREFIX}{suffix}")
}
