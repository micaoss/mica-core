//! Bluetooth settings in the tree.

use micad_settings::Settings;

/// A device that declares nothing Bluetooth writes the document its schema
/// default produces: the switch on, no devices.
#[test]
pub(super) fn bluetooth_defaults_are_on() {
    let settings = Settings::default();

    assert!(settings.bluetooth.enabled);
    assert!(settings.bluetooth.devices.is_empty());
    assert_eq!(
        serde_json::to_value(&settings.bluetooth).unwrap(),
        serde_json::json!({ "enabled": true, "provisioning": { "autoWhenOffline": true } })
    );
}

/// A document from before the pairing window and BLE provisioning still
/// loads; the keys it no longer has are dropped when it is next written.
#[test]
pub(super) fn retired_bluetooth_keys_load_and_are_not_written_again() {
    let bluetooth: micad_settings::BluetoothSettings = serde_json::from_value(serde_json::json!({
        "enabled": true, "discoverable": true, "alias": "deck", "pin": "421137",
    }))
    .expect("an earlier document loads");
    assert_eq!(
        serde_json::to_value(&bluetooth).unwrap(),
        serde_json::json!({
            "enabled": true, "provisioning": { "autoWhenOffline": true },
        })
    );
    assert!(
        serde_json::from_value::<micad_settings::BluetoothSettings>(
            serde_json::json!({ "scan": true })
        )
        .is_err(),
        "an unknown key is still refused"
    );
}

/// What a device map may hold.
#[test]
pub(super) fn a_device_map_is_refused_when_no_adapter_could_use_it() {
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
