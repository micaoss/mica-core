//! The claim that BLE provisioning and the console's setup share.

use super::*;

/// Of two claims exactly one lands: the second finds an administrator.
#[tokio::test]
pub(super) async fn a_claim_is_refused_once_the_device_has_an_administrator() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    let claim = |hash: &str| {
        vec![(
            "access.webAdmin".to_string(),
            serde_json::json!({ "password_hash": hash }),
        )]
    };

    service
        .persist_claim(&claim("first"))
        .await
        .expect("claimed");
    let refused = service.persist_claim(&claim("second")).await;

    assert!(
        matches!(refused, Err(micad_settings::SettingsError::Claimed)),
        "{refused:?}"
    );
    let hash = service.inner.read().await.settings.access.web_admin.clone();
    assert_eq!(hash.unwrap().password_hash, "first");
}

/// While a session holds the radio, a write reaching `wifi` (or the whole
/// tree) is refused; other writes go through.
#[tokio::test]
pub(super) async fn wifi_writes_are_refused_while_a_session_holds_the_radio() {
    let (service, _adapter, _dir) = service_with_bluetooth(true, true);
    let paths = |list: &[&str]| list.iter().map(|path| path.to_string()).collect::<Vec<_>>();

    service
        .provisioning
        .wifi_lock()
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for refused in [&["wifi.client"][..], &["system.hostname", "wifi"], &[""]] {
        let result = service.refuse_while_wifi_locked(&paths(refused));
        assert!(
            matches!(result, Err(crate::bus::SettingsFault::WifiLocked(_))),
            "{refused:?}: {result:?}"
        );
    }
    assert!(
        service
            .refuse_while_wifi_locked(&paths(&["system.hostname"]))
            .is_ok()
    );

    service
        .provisioning
        .wifi_lock()
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        service
            .refuse_while_wifi_locked(&paths(&["wifi.client"]))
            .is_ok()
    );
}

/// BLE's `ap`: the access point while it is on, with the key on the label
/// only while the device is unclaimed and runs on it. A key the user set is
/// never sent, and none after the claim.
#[test]
pub(super) fn the_label_key_is_shown_over_ble_only_while_unclaimed() {
    use crate::bus::provisioning::ap_status;
    use serde_json::json;
    let derived = json!({ "ssid": "mica-JXHD", "pskSource": "derived" });
    let label = || Some("W3Q3ZERVUME6L2ZV".to_string());

    assert_eq!(ap_status(false, false, Some(&derived), label), None);
    assert_eq!(
        ap_status(true, false, Some(&derived), label),
        Some(json!({ "on": true, "ssid": "mica-JXHD", "psk": "W3Q3ZERVUME6L2ZV" }))
    );
    assert_eq!(
        ap_status(true, true, Some(&derived), label),
        Some(json!({ "on": true, "ssid": "mica-JXHD", "psk": null }))
    );
    let own = json!({ "ssid": "Harbour AP", "pskSource": "settings" });
    let never = || -> Option<String> { panic!("a user-set key is never read") };
    assert_eq!(
        ap_status(true, false, Some(&own), never),
        Some(json!({ "on": true, "ssid": "Harbour AP", "psk": null }))
    );
    assert_eq!(
        ap_status(true, false, None, never),
        Some(json!({ "on": true, "ssid": null, "psk": null }))
    );
}
