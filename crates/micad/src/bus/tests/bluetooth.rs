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
    // The default code, because the tree declares none.
    assert_eq!(value["pin"], serde_json::json!("000000"));
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

/// A pairing the peer started is declared once the operator accepts it: the
/// reconciler removes a paired device nobody declares, so an accepted pairing
/// left undeclared would be undone by the next pass.
#[tokio::test]
pub(super) async fn an_accepted_peer_pairing_is_declared_trusted() {
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
    assert!(declared["AA:BB:CC:DD:EE:01"].trusted);
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

const ADMIN_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA";

/// Until the device has an administrator its pairing code is the default
/// one, which setup over Bluetooth needs; the claim replaces it with six
/// random digits, which the agent answers from then on.
#[tokio::test]
pub(super) async fn the_claim_replaces_the_default_pairing_code_with_six_random_digits() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service
        .write_setting("hostname", serde_json::json!("edge-1"))
        .await
        .expect("an unrelated write");
    assert!(service.inner.read().await.settings.bluetooth.pin.is_none());

    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": ADMIN_HASH }),
        )
        .await
        .expect("claim");

    let pin = service
        .inner
        .read()
        .await
        .settings
        .bluetooth
        .pin
        .clone()
        .expect("the claim declares a code");
    assert_eq!(pin.len(), 6, "{pin}");
    assert!(pin.bytes().all(|byte| byte.is_ascii_digit()), "{pin}");
    assert_ne!(pin, micad_settings::DEFAULT_PAIRING_PIN);

    // A later password change is not a claim: the code stays.
    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": format!("{ADMIN_HASH}x") }),
        )
        .await
        .expect("password change");
    assert_eq!(
        service.inner.read().await.settings.bluetooth.pin.as_deref(),
        Some(pin.as_str())
    );
}

/// A code somebody declared is theirs: the claim keeps it. One declared as
/// the default is the default, and is sealed like an absent one.
#[tokio::test]
pub(super) async fn the_claim_keeps_a_declared_pairing_code() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service
        .write_setting("bluetooth.pin", serde_json::json!("471100"))
        .await
        .expect("declare a code");
    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": ADMIN_HASH }),
        )
        .await
        .expect("claim");
    assert_eq!(
        service.inner.read().await.settings.bluetooth.pin.as_deref(),
        Some("471100")
    );

    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    service
        .write_setting("bluetooth.pin", serde_json::json!("000000"))
        .await
        .expect("declare the default");
    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": ADMIN_HASH }),
        )
        .await
        .expect("claim");
    assert_ne!(
        service.inner.read().await.settings.bluetooth.pairing_pin(),
        micad_settings::DEFAULT_PAIRING_PIN
    );
}

/// Bluetooth switched off at the claim leaves the code alone; switching it on
/// afterwards, on a claimed device, is when the code is sealed.
#[tokio::test]
pub(super) async fn the_code_is_sealed_only_once_bluetooth_is_on() {
    let (service, _adapter, _dir) = service_with_bluetooth(false, true);
    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": ADMIN_HASH }),
        )
        .await
        .expect("claim");
    assert!(service.inner.read().await.settings.bluetooth.pin.is_none());

    service
        .write_setting("bluetooth.enabled", serde_json::json!(true))
        .await
        .expect("switch on");
    let pin = service.inner.read().await.settings.bluetooth.pin.clone();
    assert!(pin.as_ref().is_some_and(|pin| pin.len() == 6), "{pin:?}");
}

/// A product without Bluetooth gets no code at all.
#[tokio::test]
pub(super) async fn a_product_without_bluetooth_gets_no_code() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    let service = service.with_features(micad_settings::Features::only(&[
        micad_settings::Feature::Wifi,
    ]));
    service
        .write_setting(
            "access.webAdmin",
            serde_json::json!({ "password_hash": ADMIN_HASH }),
        )
        .await
        .expect("claim");
    assert!(service.inner.read().await.settings.bluetooth.pin.is_none());
}
