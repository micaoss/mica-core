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
