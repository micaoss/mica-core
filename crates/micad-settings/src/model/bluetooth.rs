//! Bluetooth settings: the adapter, the pairing PIN and trusted devices.

use std::collections::BTreeMap;

/// Bluetooth policy: whether the adapter runs, how it presents itself, and
/// which devices it trusts.
///
/// Every field is declared. What BlueZ currently sees -- which devices are in
/// range, which are connected -- is observed and lives nowhere in this tree:
/// a paired phone that is switched off is still a declared device, and a phone
/// in range that nobody paired is not one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BluetoothSettings {
    /// Whether `bluetooth.service` runs and the adapter is powered.
    ///
    /// On by default: a powered adapter that is not discoverable is seen by
    /// nobody, and a phone can pair as soon as the operator starts a pairing.
    pub enabled: bool,
    /// Whether the adapter answers a scan.
    ///
    /// Off by default: a device that is discoverable is one anybody in range
    /// can see. Pairing turns it on for as long as the operator is pairing.
    pub discoverable: bool,
    /// The name the adapter advertises; absent advertises the hostname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// The pairing code offered to a peer that asks for one.
    ///
    /// **Displayed, not hidden.** A legacy peer asks the device for a code and
    /// somebody has to type it on the peer's keypad, so this value is shown in
    /// the console by design and is not a secret. Absent is
    /// [`DEFAULT_PAIRING_PIN`]: the code every unclaimed device offers, so a
    /// phone can set one up over Bluetooth before it has an administrator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// The devices this device has paired with, by address.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub devices: BTreeMap<String, PairedDevice>,
}

impl Default for BluetoothSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            discoverable: false,
            alias: None,
            pin: None,
            devices: BTreeMap::new(),
        }
    }
}

/// One paired device, as the settings tree holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PairedDevice {
    /// What it called itself when it paired; empty when it offered no name.
    pub name: String,
    /// Whether it may reconnect without being confirmed again.
    pub trusted: bool,
    /// Whether the adapter refuses it.
    pub blocked: bool,
}

/// How many digits a pairing code carries.
///
/// Every keypad that will be asked to enter one has digits and nothing else,
/// and six is what a passkey holds.
pub(super) const PIN_LEN: usize = 6;

/// The pairing code of a device that declares none: the one a phone setting
/// up an unclaimed device knows without being told.
pub const DEFAULT_PAIRING_PIN: &str = "000000";

impl BluetoothSettings {
    /// The pairing code this device offers: the declared one, or
    /// [`DEFAULT_PAIRING_PIN`].
    #[must_use]
    pub fn pairing_pin(&self) -> &str {
        self.pin.as_deref().unwrap_or(DEFAULT_PAIRING_PIN)
    }
}

/// Refuse a pairing code no keypad could enter.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_pairing_pin(pin: &str) -> Result<(), String> {
    if pin.len() != PIN_LEN || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("a pairing code is {PIN_LEN} digits"));
    }
    Ok(())
}

/// Refuse a device map no adapter could hold.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_bluetooth(settings: &BluetoothSettings) -> Result<(), String> {
    if let Some(pin) = &settings.pin {
        validate_pairing_pin(pin)?;
    }
    if let Some(alias) = &settings.alias
        && (alias.is_empty() || alias.len() > 64)
    {
        return Err("a Bluetooth alias is 1 to 64 characters".to_string());
    }
    for address in settings.devices.keys() {
        if !is_bluetooth_address(address) {
            return Err(format!(
                "{address:?} is not a Bluetooth address: six hexadecimal octets separated by colons, such as `AA:BB:CC:DD:EE:FF`"
            ));
        }
    }
    Ok(())
}

/// Whether `value` is the `AA:BB:CC:DD:EE:FF` an adapter names a device by.
#[must_use]
pub fn is_bluetooth_address(value: &str) -> bool {
    let octets: Vec<&str> = value.split(':').collect();
    octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
