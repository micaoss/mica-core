//! The Bluetooth members.

use super::*;

/// The read names the declared list, the adapter, the devices and the code
/// a legacy peer will be told -- apart, never merged.
#[tokio::test]
pub(super) async fn the_bluetooth_read_names_each_side_and_the_pairing_code() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);

    let value: serde_json::Value =
        serde_json::from_str(&service.get_bluetooth().await.expect("read"))
            .expect("bluetooth JSON");

    assert_eq!(value["enabled"], serde_json::json!(true));
    assert_eq!(value["adapter"]["available"], serde_json::json!(true));
    assert_eq!(
        value["devices"]["entries"][0]["address"],
        serde_json::json!("AA:BB:CC:DD:EE:01")
    );
    assert_eq!(value["declared"], serde_json::json!({}));
    // Derived from this device's identifier, because the tree declares
    // none: fixed for this device and not shared with the fleet.
    assert_eq!(
        value["pin"],
        serde_json::json!(micad_settings::derived_pairing_pin(
            "0123456789abcdef0123456789abcdef"
        ))
    );
    assert!(value["pending"].is_null());
}

/// Pairing records the device in the trust list, trusted: an operator who
/// confirmed a passkey has said yes to this device.
#[tokio::test]
pub(super) async fn pairing_records_the_device_as_trusted() {
    let (service, adapter, _dir) = service_with_bluetooth(true, true);

    service
        .pair_bluetooth_device("AA:BB:CC:DD:EE:01")
        .await
        .expect("pair");

    assert!(
        adapter
            .calls
            .lock()
            .expect("calls")
            .contains(&"pair AA:BB:CC:DD:EE:01".to_string())
    );
    let declared = &service.inner.read().await.settings.bluetooth.devices;
    assert!(declared["AA:BB:CC:DD:EE:01"].trusted);
    assert_eq!(declared["AA:BB:CC:DD:EE:01"].name, "phone");
}

/// Every action is refused on a device whose Bluetooth is off, and on one
/// with no adapter -- with the reason, not a bus error about an object
/// that does not exist.
#[tokio::test]
pub(super) async fn bluetooth_actions_are_refused_without_an_adapter_or_with_the_switch_off() {
    let (off, adapter, _dir) = service_with_bluetooth(false, true);
    let error = off
        .set_bluetooth_discovery(true)
        .await
        .expect_err("switched off");
    assert!(format!("{error}").contains("bluetooth.enabled"), "{error}");
    assert!(adapter.calls.lock().expect("calls").is_empty());

    let (absent, adapter, _dir) = service_with_bluetooth(true, false);
    let error = absent
        .pair_bluetooth_device("AA:BB:CC:DD:EE:01")
        .await
        .expect_err("no adapter");
    assert!(format!("{error}").contains("no adapter"), "{error}");
    assert!(adapter.calls.lock().expect("calls").is_empty());
}

/// An address no adapter could name is refused before any call.
#[tokio::test]
pub(super) async fn a_pair_of_something_that_is_not_an_address_is_refused() {
    let (service, adapter, _dir) = service_with_bluetooth(true, true);

    let error = service
        .pair_bluetooth_device("not-an-address")
        .await
        .expect_err("not an address");

    assert!(
        format!("{error}").contains("not a Bluetooth address"),
        "{error}"
    );
    assert!(adapter.calls.lock().expect("calls").is_empty());
}

/// Removing takes the declaration and tells the adapter now, rather than
/// leaving the keys until the next reconcile.
#[tokio::test]
pub(super) async fn removing_a_device_drops_the_declaration_and_the_keys() {
    let (service, adapter, _dir) = service_with_bluetooth(true, true);
    service
        .pair_bluetooth_device("AA:BB:CC:DD:EE:01")
        .await
        .expect("pair");

    service
        .remove_bluetooth_device("AA:BB:CC:DD:EE:01")
        .await
        .expect("remove");

    assert!(
        service
            .inner
            .read()
            .await
            .settings
            .bluetooth
            .devices
            .is_empty()
    );
    assert!(
        adapter
            .calls
            .lock()
            .expect("calls")
            .contains(&"remove AA:BB:CC:DD:EE:01".to_string())
    );
    // And a device nobody declared cannot be removed.
    let error = service
        .remove_bluetooth_device("AA:BB:CC:DD:EE:02")
        .await
        .expect_err("not declared");
    assert!(format!("{error}").contains("no device named"), "{error}");
}

/// A confirmation with nothing waiting is refused rather than silently
/// accepted: the console would otherwise report a pairing that never
/// happened.
#[tokio::test]
pub(super) async fn a_confirmation_with_nothing_waiting_is_refused() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);

    let error = service
        .confirm_bluetooth_pairing("AA:BB:CC:DD:EE:01", true)
        .await
        .expect_err("nothing waiting");

    assert!(
        format!("{error}").contains("no pairing confirmation"),
        "{error}"
    );
}
