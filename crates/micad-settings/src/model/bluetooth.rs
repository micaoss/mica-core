//! Bluetooth settings: the adapter, the pairing PIN, trusted devices and BLE
//! provisioning.
//!
//! The adapter is never discoverable or pairable by a setting: an operator
//! opens a pairing window for three minutes instead (micad's `bluetooth`
//! module), and the adapter advertises the hostname.

use std::collections::BTreeMap;

/// Bluetooth policy: whether the adapter runs and which devices it trusts.
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
    /// The code `mica-bled` authenticates the phone app with.
    ///
    /// **Displayed, not hidden**: somebody types it into the app. No Bluetooth
    /// pairing is answered with it. Absent is [`DEFAULT_PAIRING_PIN`]: the
    /// code every unclaimed device offers, so a phone can set one up before
    /// it has an administrator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// The devices this device has paired with, by address.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub devices: BTreeMap<String, PairedDevice>,
    /// BLE provisioning (`docs/ble-provisioning.md`).
    pub provisioning: BleProvisioningSettings,
    /// `discoverable` and `alias`, which earlier documents carry: read and
    /// dropped at the next save.
    #[serde(skip_serializing)]
    pub discoverable: Retired,
    #[serde(skip_serializing)]
    pub alias: Retired,
}

/// A key an earlier schema had and this one does not: accepted with any
/// value, so a document written before it was retired still loads, and never
/// written again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Retired;

impl<'de> serde::Deserialize<'de> for Retired {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self)
    }
}

impl Default for BluetoothSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            pin: None,
            devices: BTreeMap::new(),
            provisioning: BleProvisioningSettings::default(),
            discoverable: Retired,
            alias: Retired,
        }
    }
}

/// BLE provisioning's one setting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BleProvisioningSettings {
    /// Whether a claimed device that had a default route this boot and lost
    /// it for 120 seconds opens a provisioning window (`offline`). On by
    /// default.
    #[serde(rename = "autoWhenOffline")]
    pub auto_when_offline: bool,
}

impl Default for BleProvisioningSettings {
    fn default() -> Self {
        Self {
            auto_when_offline: true,
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
