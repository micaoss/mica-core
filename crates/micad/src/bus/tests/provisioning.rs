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
