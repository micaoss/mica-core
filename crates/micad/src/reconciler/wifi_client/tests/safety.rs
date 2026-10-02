//! What the reconciler never leaks and never writes outside.

use super::*;

#[tokio::test]
pub(super) async fn the_psk_reaches_the_config_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(client(
            true,
            vec![network("office", Some(SECRET_PSK), true, 4)],
        )))
        .await
        .unwrap();

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
        state["networks"],
        json!([{ "ssid": "office", "hidden": true, "priority": 4, "secured": true }]),
        "the live state reports that a key exists, never what it is"
    );
}

#[tokio::test]
pub(super) async fn an_open_network_is_reported_unsecured() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(client(
            true,
            vec![network("cafe", None, false, 0)],
        )))
        .await
        .unwrap();

    assert_eq!(state["networks"][0]["secured"], json!(false));
}

#[tokio::test]
pub(super) async fn an_unrenderable_key_fails_before_anything_is_written_or_started() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let err = reconciler
        .apply(&settings_with(client(
            true,
            vec![network("office", Some("bad\"key\nMORE"), false, 0)],
        )))
        .await
        .unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("pre-shared key"), "{chain}");
    assert!(!chain.contains("MORE"), "the key leaked: {chain}");
    assert!(!paths.config.exists());
    assert!(reconciler.control.calls().is_empty());
}

// ---- interface validation ---------------------------------------------

#[test]
pub(super) fn interface_names_that_are_not_names_are_rejected() {
    for bad in [
        "",
        "..",
        ".",
        "../../etc/passwd",
        "wlan0/../evil",
        "wlan 0",
        "wlan0\n",
        "wlan0;reboot",
        "sixteencharsnam",
        "seventeen_charsx",
    ] {
        if bad == "sixteencharsnam" {
            assert!(validate_interface(bad).is_ok(), "{bad:?} is 15 characters");
            continue;
        }
        assert!(
            validate_interface(bad).is_err(),
            "{bad:?} was accepted as an interface name"
        );
    }
    for good in ["wlan0", "wlp2s0", "wlan0.1", "wl-an_0", "eth0:1"] {
        assert!(validate_interface(good).is_ok(), "{good:?} was rejected");
    }
}

#[tokio::test]
pub(super) async fn a_traversing_interface_name_writes_nothing_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let err = reconciler
        .apply(&settings_with(WifiClientSettings {
            interface: "../../evil".to_string(),
            ..one_psk_network()
        }))
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("wifi.client.interface"),
        "{err:#}"
    );
    assert!(
        !paths.config_dir.exists() && !paths.network_dir.exists(),
        "a rejected interface name must not create anything"
    );
    assert!(reconciler.control.calls().is_empty());
}

// ---- containment ------------------------------------------------------

#[tokio::test]
pub(super) async fn every_path_the_reconciler_writes_stays_inside_the_tempdir() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(multi_network_client()))
        .await
        .unwrap();

    for path in [&paths.config, &paths.config_dir, &paths.network_dir] {
        assert!(
            path.starts_with(dir.path()),
            "{} escapes the tempdir",
            path.display()
        );
    }
    assert_ne!(
        state["config"],
        json!(format!("{DEFAULT_CONFIG_DIR}/wpa_supplicant-wlan0.conf")),
        "a test must never name the host's wpa_supplicant configuration"
    );
}
