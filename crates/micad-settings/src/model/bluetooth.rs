//! Bluetooth settings: the adapter, the pairing PIN and trusted devices.

use std::collections::BTreeMap;

/// Bluetooth policy: whether the adapter runs, how it presents itself, and
/// which devices it trusts.
///
/// Every field is declared. What BlueZ currently sees -- which devices are in
/// range, which are connected -- is observed and lives nowhere in this tree:
/// a paired phone that is switched off is still a declared device, and a phone
/// in range that nobody paired is not one.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BluetoothSettings {
    /// Whether `bluetooth.service` runs and the adapter is powered.
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
    /// the console by design and is not a secret. Absent derives one from the
    /// device identity: fixed for this device, stable across boots, and not
    /// the same code as every other device in the fleet -- the rule
    /// [`WifiApSettings::psk`] states, applied to the one value here that has
    /// to be readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// The devices this device has paired with, by address.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub devices: BTreeMap<String, PairedDevice>,
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

/// The digits a pairing code may carry, and how many.
///
/// Bluetooth's legacy PIN is 1 to 16 characters; every keypad that will be
/// asked to enter one has digits and nothing else, so the code is digits.
pub(super) const MIN_PIN_LEN: usize = 4;
pub(super) const MAX_PIN_LEN: usize = 16;

/// The pairing code this device offers when none is declared.
///
/// Derived from the device identifier: the first [`MIN_PIN_LEN`] digits of its
/// hexadecimal, with each hex digit folded into a decimal one. Deterministic,
/// so the console and the agent always show and answer the same code without
/// storing it; per device, so a fleet does not share one PIN; and derived from
/// the identity rather than from a credential, because it is displayed.
#[must_use]
pub fn derived_pairing_pin(device_id: &str) -> String {
    let digits: String = device_id
        .bytes()
        .filter_map(|byte| (byte as char).to_digit(16))
        .map(|value| char::from_digit(value % 10, 10).unwrap_or('0'))
        .take(MIN_PIN_LEN)
        .collect();
    if digits.len() == MIN_PIN_LEN {
        digits
    } else {
        // A device with no identifier yet: the reconciler has nothing to
        // derive from, and a code that is shown has to be something.
        "0".repeat(MIN_PIN_LEN)
    }
}

/// Refuse a pairing code no keypad could enter.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_pairing_pin(pin: &str) -> Result<(), String> {
    if pin.len() < MIN_PIN_LEN || pin.len() > MAX_PIN_LEN {
        return Err(format!(
            "a pairing code is {MIN_PIN_LEN} to {MAX_PIN_LEN} digits"
        ));
    }
    if !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("a pairing code is digits: every keypad that will be asked to enter one has those and nothing else".to_string());
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
