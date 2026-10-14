//! Starting, stopping and restarting the access point.

use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
pub(super) async fn off_to_always_enables_then_starts_and_back_to_off_stops_then_disables() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let off = reconciler
        .apply(&settings_with(WifiApSettings {
            mode: ApMode::Off,
            ..lab_ap()
        }))
        .await
        .unwrap();
    assert_eq!(off["accessPoint"], json!("stopped"));
    assert!(
        reconciler.control.calls().is_empty(),
        "an already-stopped access point got calls: {:?}",
        reconciler.control.calls()
    );
    assert!(reconciler.reloader.calls().is_empty());
    assert!(
        !paths.config.exists(),
        "a stopped access point rendered a key"
    );

    let on = reconciler.apply(&settings_with(lab_ap())).await.unwrap();
    assert_eq!(on["accessPoint"], json!("applied"));
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service".to_string(),
            "start hostapd@wlan0.service".to_string(),
        ]
    );
    assert_eq!(on["activeState"], json!("active"));
    assert_eq!(on["unitFileState"], json!("enabled-runtime"));

    let back_off = reconciler
        .apply(&settings_with(WifiApSettings {
            mode: ApMode::Off,
            ..lab_ap()
        }))
        .await
        .unwrap();
    assert_eq!(back_off["accessPoint"], json!("stopped"));
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service".to_string(),
            "start hostapd@wlan0.service".to_string(),
            "stop hostapd@wlan0.service".to_string(),
            "disable hostapd@wlan0.service".to_string(),
        ]
    );
    assert_eq!(back_off["networkdUnit"], json!(null));
    assert!(
        !paths.networkd().exists(),
        "a stopped access point must not leave a DHCP server behind"
    );
}

#[tokio::test]
pub(super) async fn provisioning_mode_starts_the_access_point_exactly_as_always_does() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(WifiApSettings {
            mode: ApMode::Provisioning,
            ..lab_ap()
        }))
        .await
        .unwrap();

    assert_eq!(state["mode"], json!("provisioning"));
    assert_eq!(state["accessPoint"], json!("applied"));
    assert_eq!(
        std::fs::read_to_string(&paths.config).unwrap(),
        GOLDEN_CONFIG
    );
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service".to_string(),
            "start hostapd@wlan0.service".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn reapplying_identical_settings_is_a_no_op_with_zero_bus_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let settings = settings_with(lab_ap());

    let first = reconciler.apply(&settings).await.unwrap();
    let after_first = reconciler.control.calls();
    // A marker the reconciler would clobber if it rewrote the file: the
    // renderer always produces 0600.
    std::fs::set_permissions(&paths.config, std::fs::Permissions::from_mode(0o644)).unwrap();

    let second = reconciler.apply(&settings).await.unwrap();

    assert_eq!(
        reconciler.control.calls(),
        after_first,
        "a converged system got extra calls"
    );
    assert_eq!(
        reconciler.reloader.calls(),
        vec!["reload".to_string()],
        "networkd was reloaded for nothing"
    );
    assert_eq!(
        mode_of(&paths.config),
        0o644,
        "an unchanged configuration must not be rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.config).unwrap(),
        GOLDEN_CONFIG
    );
    assert_eq!(first["accessPoint"], json!("applied"));
    assert_eq!(second["accessPoint"], json!("unchanged"));
}

#[tokio::test]
pub(super) async fn an_already_converged_access_point_needs_no_calls_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_CONFIG).unwrap();
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    assert!(
        reconciler.control.calls().is_empty(),
        "converged system got calls: {:?}",
        reconciler.control.calls()
    );
    assert!(
        reconciler.reloader.calls().is_empty(),
        "converged system got a reload"
    );
    assert_eq!(state["accessPoint"], json!("unchanged"));
}

#[tokio::test]
pub(super) async fn changing_the_config_of_a_running_access_point_restarts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_CONFIG).unwrap();
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let state = reconciler
        .apply(&settings_with(WifiApSettings {
            ssid: Some("mica-annex".to_string()),
            ..lab_ap()
        }))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec!["restart hostapd@wlan0.service".to_string()],
        "hostapd reads its configuration once at start; without a restart \
         the radio keeps beaconing the previous SSID and key"
    );
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("ssid=mica-annex\n")
    );
    assert_eq!(state["accessPoint"], json!("applied"));
}

#[tokio::test]
pub(super) async fn changing_the_address_of_a_running_access_point_reloads_networkd_once() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_CONFIG).unwrap();
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    reconciler
        .apply(&settings_with(WifiApSettings {
            address: "10.7.0.1/16".to_string(),
            ..lab_ap()
        }))
        .await
        .unwrap();

    assert_eq!(reconciler.reloader.calls(), vec!["reload".to_string()]);
    let rendered = std::fs::read_to_string(paths.networkd()).unwrap();
    assert!(rendered.contains("Address=10.7.0.1/16\n"), "{rendered}");
    assert!(
        rendered.contains("PoolSize=65533\n"),
        "the pool must move with the address: {rendered}"
    );
}

// ---- the single-radio conflict ----------------------------------------
