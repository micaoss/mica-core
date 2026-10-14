//! Bluetooth settings in the tree.

use micad_settings::Settings;

/// A device that declares nothing Bluetooth writes the document its schema
/// default produces: the switch off, nothing discoverable, no devices.
#[test]
pub(super) fn bluetooth_defaults_are_off_and_undiscoverable() {
    let settings = Settings::default();

    assert!(!settings.bluetooth.enabled);
    assert!(!settings.bluetooth.discoverable);
    assert!(settings.bluetooth.pin.is_none());
    assert!(settings.bluetooth.devices.is_empty());
    assert_eq!(
        serde_json::to_value(&settings.bluetooth).unwrap(),
        serde_json::json!({ "enabled": false, "discoverable": false })
    );
}

/// The pairing code a device offers when none is declared: derived from its
/// identifier, so it is fixed for this device and not shared with the fleet.
#[test]
pub(super) fn the_derived_pairing_code_is_per_device_and_stable() {
    let first = micad_settings::derived_pairing_pin("0123456789abcdef0123456789abcdef");
    let second = micad_settings::derived_pairing_pin("fedcba9876543210fedcba9876543210");

    assert_eq!(
        first,
        micad_settings::derived_pairing_pin("0123456789abcdef0123456789abcdef")
    );
    assert_ne!(first, second, "two devices share one pairing code");
    assert_eq!(first.len(), 4);
    assert!(first.bytes().all(|byte| byte.is_ascii_digit()), "{first}");
    // A device with no identifier still has something to show.
    assert_eq!(micad_settings::derived_pairing_pin(""), "0000");
}

/// What a pairing code may be, and what a device map may hold.
#[test]
pub(super) fn a_pairing_code_and_a_device_map_are_refused_when_no_adapter_could_use_them() {
    assert!(micad_settings::validate_pairing_pin("0000").is_ok());
    assert!(micad_settings::validate_pairing_pin("1234567890123456").is_ok());
    assert!(micad_settings::validate_pairing_pin("123").is_err());
    assert!(micad_settings::validate_pairing_pin("12345678901234567").is_err());
    assert!(micad_settings::validate_pairing_pin("abcd").is_err());

    let mut settings = Settings::default();
    let refused = settings.set(
        "bluetooth.devices",
        serde_json::json!({ "not-an-address": { "name": "phone", "trusted": true, "blocked": false } }),
    );
    assert!(
        refused.is_err(),
        "an address no adapter could name was accepted"
    );
    assert!(settings.bluetooth.devices.is_empty());

    settings
        .set(
            "bluetooth.devices",
            serde_json::json!({ "AA:BB:CC:DD:EE:FF": { "name": "phone", "trusted": true, "blocked": false } }),
        )
        .expect("a real address is accepted");
    assert!(settings.bluetooth.devices["AA:BB:CC:DD:EE:FF"].trusted);
}
