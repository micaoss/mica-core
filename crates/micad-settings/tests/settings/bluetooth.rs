//! Bluetooth settings in the tree.

use micad_settings::Settings;

/// A device that declares nothing Bluetooth writes the document its schema
/// default produces: the switch on, no devices.
#[test]
pub(super) fn bluetooth_defaults_are_on() {
    let settings = Settings::default();

    assert!(settings.bluetooth.enabled);
    assert!(settings.bluetooth.pin.is_none());
    assert!(settings.bluetooth.devices.is_empty());
    assert_eq!(
        serde_json::to_value(&settings.bluetooth).unwrap(),
        serde_json::json!({ "enabled": true })
    );
}

/// A document from before the pairing window still loads; the keys it no
/// longer has are dropped when it is next written.
#[test]
pub(super) fn retired_bluetooth_keys_load_and_are_not_written_again() {
    let bluetooth: micad_settings::BluetoothSettings = serde_json::from_value(serde_json::json!({
        "enabled": true, "discoverable": true, "alias": "deck", "pin": "421137",
    }))
    .expect("an earlier document loads");
    assert_eq!(
        serde_json::to_value(&bluetooth).unwrap(),
        serde_json::json!({ "enabled": true, "pin": "421137" })
    );
    assert!(
        serde_json::from_value::<micad_settings::BluetoothSettings>(
            serde_json::json!({ "scan": true })
        )
        .is_err(),
        "an unknown key is still refused"
    );
}

/// The pairing code a device offers when none is declared: the default one.
#[test]
pub(super) fn an_absent_pairing_code_is_the_default_one() {
    let mut settings = Settings::default();
    assert_eq!(settings.bluetooth.pairing_pin(), "000000");
    assert_eq!(micad_settings::DEFAULT_PAIRING_PIN, "000000");

    settings.bluetooth.pin = Some("421137".to_string());
    assert_eq!(settings.bluetooth.pairing_pin(), "421137");
}

/// What a pairing code may be, and what a device map may hold.
#[test]
pub(super) fn a_pairing_code_and_a_device_map_are_refused_when_no_adapter_could_use_them() {
    assert!(micad_settings::validate_pairing_pin("421137").is_ok());
    assert!(micad_settings::validate_pairing_pin("000000").is_ok());
    assert!(micad_settings::validate_pairing_pin("4211").is_err());
    assert!(micad_settings::validate_pairing_pin("1234567").is_err());
    assert!(micad_settings::validate_pairing_pin("abcdef").is_err());
    assert!(micad_settings::validate_pairing_pin("12 456").is_err());

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
