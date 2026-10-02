//! What the reconciler never leaks and never writes outside.

use super::*;

#[tokio::test]
pub(super) async fn the_key_reaches_the_config_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    seed_ap_psk(&paths.state_dir, SECRET_PSK);
    let ap = WifiApSettings {
        psk: None,
        ssid: None,
        ..lab_ap()
    };

    let state = reconciler.apply(&settings_with(ap)).await.unwrap();

    // Anti-tautology: the key must genuinely have been applied, or every
    // absence below would be vacuous.
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains(SECRET_PSK),
        "the key never reached the configuration, so this test proves nothing"
    );
    let rendered_state = serde_json::to_string(&state).unwrap();
    assert!(
        !rendered_state.contains(SECRET_PSK),
        "the key is in the live-state tree, which is served over D-Bus: {rendered_state}"
    );
    assert!(
        !std::fs::read_to_string(paths.networkd())
            .unwrap()
            .contains(SECRET_PSK)
    );
    assert!(
        !reconciler.control.calls().join(" ").contains(SECRET_PSK),
        "the key reached a unit name"
    );
    assert_eq!(
        state["secured"],
        json!(true),
        "the live state reports that the access point is protected, never how"
    );
}

#[tokio::test]
pub(super) async fn a_key_that_cannot_be_rendered_leaks_nothing_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        psk: Some(format!("bad\n{SECRET_PSK}")),
        ..lab_ap()
    };

    let err = reconciler.apply(&settings_with(ap)).await.unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("pre-shared key"), "{chain}");
    assert!(!chain.contains(SECRET_PSK), "the key leaked: {chain}");
    assert!(!paths.config.exists());
    assert!(reconciler.control.calls().is_empty());
}

// ---- interface validation and containment -----------------------------

#[test]
pub(super) fn interface_names_that_are_not_names_are_rejected() {
    for bad in [
        "",
        ".",
        "..",
        "../../etc/passwd",
        "wlan0/../evil",
        "wlan 0",
        "wlan0\n",
        "wlan0;reboot",
        "seventeen_charsx",
    ] {
        assert!(
            validate_interface(bad).is_err(),
            "{bad:?} was accepted as an interface name"
        );
    }
    for good in [
        "wlan0",
        "wlp2s0",
        "wlan0.1",
        "wl-an_0",
        "eth0:1",
        "sixteencharsnam",
    ] {
        assert!(validate_interface(good).is_ok(), "{good:?} was rejected");
    }
}

#[tokio::test]
pub(super) async fn a_traversing_interface_name_writes_nothing_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let err = reconciler
        .apply(&settings_with(WifiApSettings {
            interface: "../../evil".to_string(),
            ..lab_ap()
        }))
        .await
        .unwrap_err();

    assert!(format!("{err:#}").contains("wifi.ap.interface"), "{err:#}");
    assert!(
        !paths.config_dir.exists() && !paths.network_dir.exists(),
        "a rejected interface name must not create anything"
    );
    assert!(reconciler.control.calls().is_empty());
}

#[tokio::test]
pub(super) async fn every_path_the_reconciler_writes_stays_inside_the_tempdir() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    for path in [
        &paths.config,
        &paths.config_dir,
        &paths.network_dir,
        &paths.state_dir,
    ] {
        assert!(
            path.starts_with(dir.path()),
            "{} escapes the tempdir",
            path.display()
        );
    }
    assert_ne!(
        state["config"],
        json!(format!("{DEFAULT_CONFIG_DIR}/wlan0.conf")),
        "a test must never name the host's hostapd configuration"
    );
}
