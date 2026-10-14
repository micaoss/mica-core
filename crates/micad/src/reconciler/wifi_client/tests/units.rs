//! Starting, stopping and restarting the supplicant.

use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
pub(super) async fn disabled_to_enabled_enables_then_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable wpa_supplicant@wlan0.service".to_string(),
            "start wpa_supplicant@wlan0.service".to_string(),
        ]
    );
    assert_eq!(state["unit"], json!("wpa_supplicant@wlan0.service"));
    assert_eq!(state["activeState"], json!("active"));
    assert_eq!(state["unitFileState"], json!("enabled-runtime"));
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["interface"], json!("wlan0"));
}

#[tokio::test]
pub(super) async fn enabled_to_disabled_stops_then_disables_and_drops_the_networkd_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let state = reconciler
        .apply(&settings_with(client(
            false,
            vec![network("office", Some("officepass"), false, 10)],
        )))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "stop wpa_supplicant@wlan0.service".to_string(),
            "disable wpa_supplicant@wlan0.service".to_string(),
        ]
    );
    assert_eq!(state["station"], json!("disabled"));
    assert_eq!(state["networkdUnit"], json!(null));
    assert!(!paths.networkd().exists());
    assert_eq!(reconciler.reloader.calls(), vec!["reload".to_string()]);
}

#[tokio::test]
pub(super) async fn reapplying_identical_settings_is_a_no_op_with_zero_bus_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let settings = settings_with(multi_network_client());

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
        GOLDEN_MULTI
    );
    assert_eq!(first["station"], json!("applied"));
    assert_eq!(second["station"], json!("unchanged"));
}

#[tokio::test]
pub(super) async fn an_already_converged_enabled_station_needs_no_calls_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_MULTI).unwrap();
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let state = reconciler
        .apply(&settings_with(multi_network_client()))
        .await
        .unwrap();

    assert!(
        reconciler.control.calls().is_empty(),
        "converged system got calls: {:?}",
        reconciler.control.calls()
    );
    assert!(
        reconciler.reloader.calls().is_empty(),
        "converged system got a reload"
    );
    assert_eq!(state["station"], json!("unchanged"));
}

#[tokio::test]
pub(super) async fn an_already_stopped_disabled_station_needs_no_calls_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_EMPTY).unwrap();

    reconciler
        .apply(&settings_with(client(false, Vec::new())))
        .await
        .unwrap();

    assert!(
        reconciler.control.calls().is_empty(),
        "converged system got calls: {:?}",
        reconciler.control.calls()
    );
    assert!(reconciler.reloader.calls().is_empty());
}

#[tokio::test]
pub(super) async fn changing_the_config_of_a_running_supplicant_restarts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_MULTI).unwrap();
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let mut changed = multi_network_client();
    changed
        .networks
        .push(network("annex", Some("annexpass"), false, 1));
    let state = reconciler.apply(&settings_with(changed)).await.unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec!["restart wpa_supplicant@wlan0.service".to_string()],
        "wpa_supplicant reads its configuration once at start; without a \
         restart the new file never takes effect"
    );
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("\tssid=\"annex\"\n")
    );
    assert_eq!(state["station"], json!("applied"));
}

#[tokio::test]
pub(super) async fn changing_the_config_of_a_stopped_supplicant_does_not_start_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(&paths.config, GOLDEN_EMPTY).unwrap();

    reconciler
        .apply(&settings_with(client(
            false,
            vec![network("office", Some("officepass"), false, 10)],
        )))
        .await
        .unwrap();

    assert!(
        reconciler.control.calls().is_empty(),
        "a disabled station must stay down however much its config moved: {:?}",
        reconciler.control.calls()
    );
}

// ---- enabled with nothing to join -------------------------------------

#[tokio::test]
pub(super) async fn enabled_with_no_networks_stays_down_and_reports_idle() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    std::fs::create_dir_all(&paths.network_dir).unwrap();
    std::fs::write(paths.networkd(), GOLDEN_NETWORKD).unwrap();

    let state = reconciler
        .apply(&settings_with(client(true, Vec::new())))
        .await
        .unwrap();

    assert_eq!(
        state["station"],
        json!("idle"),
        "enabled-but-unconfigured must not read as disabled"
    );
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "stop wpa_supplicant@wlan0.service".to_string(),
            "disable wpa_supplicant@wlan0.service".to_string(),
        ],
        "a supplicant with no networks would claim the radio without ever associating"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.config).unwrap(),
        GOLDEN_EMPTY,
        "the configuration is still rendered, so adding the first network \
         is an ordinary change"
    );
    assert!(!paths.networkd().exists());
}

// ---- secret hygiene ---------------------------------------------------
