//! The Bluetooth members.

use super::*;

/// The read names the declared list, the adapter and the devices -- apart,
/// never merged -- and offers no pairing code.
#[tokio::test]
pub(super) async fn the_bluetooth_read_names_each_side() {
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
    assert!(value.get("pin").is_none());
    assert_eq!(value["pending"], serde_json::json!([]));
    assert!(value["pairWindow"].is_null());
}

/// The pairing window opens the adapter, shows what waits by name, MAC and
/// six digits, and closing it refuses what still waits.
#[tokio::test]
pub(super) async fn the_pairing_window_opens_shows_and_closes() {
    let (service, adapter, _dir) = service_with_bluetooth(true, true);
    let expires = service.open_bluetooth_pair_window().await.expect("open");
    let agent = Arc::clone(&service.agent);
    let waiting = tokio::spawn(async move {
        agent
            .confirm("AA:BB:CC:DD:EE:01".to_string(), Some(42_137))
            .await
    });
    while service.agent.pending().await.is_empty() {
        tokio::task::yield_now().await;
    }

    let value: serde_json::Value =
        serde_json::from_str(&service.get_bluetooth().await.expect("read")).expect("JSON");
    assert_eq!(value["pairWindow"]["expiresAt"], serde_json::json!(expires));
    assert_eq!(value["pending"][0]["name"], serde_json::json!("phone"));
    assert_eq!(
        value["pending"][0]["mac"],
        serde_json::json!("AA:BB:CC:DD:EE:01")
    );
    assert_eq!(value["pending"][0]["passkey"], serde_json::json!("042137"));

    service.close_bluetooth_pair_window().await.expect("close");
    assert!(!waiting.await.expect("agent"), "BlueZ is told no");
    let calls = adapter.calls.lock().unwrap().clone();
    assert!(calls.contains(&"pair window true".to_string()), "{calls:?}");
    assert!(
        calls.contains(&"pair window false".to_string()),
        "{calls:?}"
    );
}

/// Switched off, there is no window to open.
#[tokio::test]
pub(super) async fn a_switched_off_adapter_opens_no_window() {
    let (service, _adapter, _dir) = service_with_bluetooth(false, true);
    assert!(service.open_bluetooth_pair_window().await.is_err());
    assert_eq!(service.agent.window().await, None);
}

/// Pairing records the device in the trust list, untrusted: trusting it is
/// the operator's further yes.
#[tokio::test]
pub(super) async fn pairing_records_the_device_untrusted() {
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
    assert!(!declared["AA:BB:CC:DD:EE:01"].trusted);
    assert_eq!(declared["AA:BB:CC:DD:EE:01"].name, "phone");
}

/// Trust and block are the operator's, one flag at a time, on a declared
/// device only.
#[tokio::test]
pub(super) async fn a_declared_device_is_trusted_and_blocked_by_flag() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service
        .write_setting(
            "bluetooth.devices",
            serde_json::json!({ "AA:BB:CC:DD:EE:01": { "name": "phone" } }),
        )
        .await
        .expect("declare");

    service
        .set_bluetooth_device_flag("AA:BB:CC:DD:EE:01", "trusted", true)
        .await
        .expect("trust");
    service
        .set_bluetooth_device_flag("AA:BB:CC:DD:EE:01", "blocked", true)
        .await
        .expect("block");
    let device = service.inner.read().await.settings.bluetooth.devices["AA:BB:CC:DD:EE:01"].clone();
    assert!(device.trusted && device.blocked, "{device:?}");

    service
        .set_bluetooth_device_flag("AA:BB:CC:DD:EE:01", "trusted", false)
        .await
        .expect("untrust");
    assert!(!service.inner.read().await.settings.bluetooth.devices["AA:BB:CC:DD:EE:01"].trusted);

    for (address, flag) in [
        ("AA:BB:CC:DD:EE:02", "trusted"),
        ("AA:BB:CC:DD:EE:01", "paired"),
    ] {
        let refused = service.set_bluetooth_device_flag(address, flag, true).await;
        assert!(
            matches!(refused, Err(zbus::fdo::Error::InvalidArgs(_))),
            "{refused:?}"
        );
    }
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

/// A pairing the peer started is declared once the operator accepts it: the
/// reconciler removes a paired device nobody declares, so an accepted pairing
/// left undeclared would be undone by the next pass.
#[tokio::test]
pub(super) async fn an_accepted_peer_pairing_is_declared_untrusted() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service.agent.open_window().await;
    let agent = Arc::clone(&service.agent);
    let waiting = tokio::spawn(async move {
        agent
            .confirm("AA:BB:CC:DD:EE:01".to_string(), Some(123_456))
            .await
    });
    while service.agent.pending().await.is_empty() {
        tokio::task::yield_now().await;
    }

    service
        .confirm_bluetooth_pairing("AA:BB:CC:DD:EE:01", true)
        .await
        .expect("answered");

    assert!(waiting.await.expect("agent"), "BlueZ is told yes");
    let declared = &service.inner.read().await.settings.bluetooth.devices;
    assert!(!declared["AA:BB:CC:DD:EE:01"].trusted);
    assert_eq!(declared["AA:BB:CC:DD:EE:01"].name, "phone");
}

/// A refused one declares nothing.
#[tokio::test]
pub(super) async fn a_refused_peer_pairing_declares_nothing() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service.agent.open_window().await;
    let agent = Arc::clone(&service.agent);
    let waiting = tokio::spawn(async move {
        agent
            .confirm("AA:BB:CC:DD:EE:01".to_string(), Some(123_456))
            .await
    });
    while service.agent.pending().await.is_empty() {
        tokio::task::yield_now().await;
    }

    service
        .confirm_bluetooth_pairing("AA:BB:CC:DD:EE:01", false)
        .await
        .expect("answered");

    assert!(!waiting.await.expect("agent"), "BlueZ is told no");
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
