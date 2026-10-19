//! The SSID and key derived from the device's identity.

use super::*;

#[test]
pub(super) fn the_derived_ssid_is_stable_and_distinct_per_device() {
    assert_eq!(derived_ssid(DEVICE_ID), derived_ssid(DEVICE_ID));
    assert_eq!(derived_ssid(DEVICE_ID), "mica-1a2b3c4d");
    assert_ne!(
        derived_ssid(DEVICE_ID),
        derived_ssid("ffffffffffffffffffffffffffffffff"),
        "two devices would advertise the same SSID"
    );
    assert!(
        validate_ssid(&derived_ssid(DEVICE_ID)).is_ok(),
        "a derived SSID must itself be a legal SSID"
    );
}

#[tokio::test]
pub(super) async fn an_absent_ssid_is_derived_from_the_device_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        ssid: None,
        ..lab_ap()
    };

    let state = reconciler.apply(&settings_with(ap)).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-1a2b3c4d"));
    assert_eq!(state["ssidSource"], json!("derived"));
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("ssid=mica-1a2b3c4d\n")
    );
}

#[tokio::test]
pub(super) async fn a_configured_ssid_is_used_verbatim_and_never_derived() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-lab"));
    assert_eq!(state["ssidSource"], json!("settings"));
    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(rendered.contains("ssid=mica-lab\n"), "{rendered}");
    assert!(
        !rendered.contains("mica-1a2b3c4d"),
        "the configured SSID was overridden by the derived one: {rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_device_with_no_identity_and_no_ssid_refuses_to_start_the_radio() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let mut settings = settings_with(WifiApSettings {
        ssid: None,
        ..lab_ap()
    });
    settings.provisioning.device_id = None;

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(format!("{err:#}").contains("wifi.ap.ssid"), "{err:#}");
    assert!(!paths.config.exists());
    assert!(reconciler.control.calls().is_empty());
}

// ---- where the key comes from -----------------------------------------

#[tokio::test]
pub(super) async fn an_absent_key_comes_from_the_per_device_state_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    seed_ap_psk(&paths.state_dir, SECRET_PSK);
    let ap = WifiApSettings {
        psk: None,
        ..lab_ap()
    };

    reconciler.apply(&settings_with(ap)).await.unwrap();

    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains(&format!("wpa_passphrase={SECRET_PSK}\n")),
        "the per-device key on STATE must be what the access point uses"
    );
}

#[tokio::test]
pub(super) async fn a_configured_key_wins_over_the_state_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    seed_ap_psk(&paths.state_dir, SECRET_PSK);

    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(
        rendered.contains("wpa_passphrase=labsecret1\n"),
        "{rendered}"
    );
    assert!(
        !rendered.contains(SECRET_PSK),
        "the settings key was ignored in favour of the state secret: {rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_device_with_no_key_anywhere_refuses_rather_than_using_a_constant() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        psk: None,
        ..lab_ap()
    };

    let err = reconciler.apply(&settings_with(ap)).await.unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("fleet-wide"), "{chain}");
    assert!(
        !paths.config.exists(),
        "a configuration was written without a key"
    );
    assert!(reconciler.control.calls().is_empty());
}

// ---- address and DHCP pool --------------------------------------------
