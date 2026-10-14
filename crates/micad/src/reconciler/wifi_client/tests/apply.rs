//! Writing the configuration and the networkd unit.

use super::super::super::network::{NetworkReconciler, NoDelete};
use crate::wgkeys::Keystore;
use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
pub(super) async fn apply_writes_the_golden_config_at_0600_creating_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(multi_network_client()))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(&paths.config).unwrap(),
        GOLDEN_MULTI
    );
    assert_eq!(
        mode_of(&paths.config),
        0o600,
        "the file carries PSKs and must not be readable by anyone else"
    );
    assert_eq!(state["station"], json!("applied"));
    assert_eq!(state["config"], json!(paths.config.display().to_string()));
    assert_eq!(reconciler.name(), "wifiClient");
    assert_eq!(reconciler.subtree(), "wifi.client");
}

#[tokio::test]
pub(super) async fn a_leftover_temporary_file_does_not_widen_the_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    // The shape an interrupted run leaves behind: the temporary name
    // already exists, so the mode given at open time is never applied.
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let leftover = paths
        .config_dir
        .join(".wpa_supplicant-wlan0.conf.micad-tmp");
    std::fs::write(&leftover, "stale").unwrap();
    std::fs::set_permissions(&leftover, std::fs::Permissions::from_mode(0o666)).unwrap();

    reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();

    assert_eq!(mode_of(&paths.config), 0o600);
    assert!(
        !leftover.exists(),
        "the temporary file must be renamed away"
    );
}

#[tokio::test]
pub(super) async fn apply_leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();

    for directory in [&paths.config_dir, &paths.network_dir] {
        let leftovers: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains("micad-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }
}

#[tokio::test]
pub(super) async fn an_enabled_station_gets_a_networkd_unit_that_addresses_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(paths.networkd()).unwrap(),
        GOLDEN_NETWORKD,
        "association without addressing is a link that looks up and carries nothing"
    );
    assert_eq!(state["networkdUnit"], json!("90-wifi-client-wlan0.network"));
    assert_eq!(reconciler.reloader.calls(), vec!["reload".to_string()]);
}

#[tokio::test]
pub(super) async fn the_rendered_networkd_unit_survives_the_network_reconcilers_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();
    assert!(paths.networkd().exists());

    // The network reconciler deletes every `*-mica-*.network` it did not
    // itself render. Sharing a directory with it means the station's unit
    // has to be outside that pattern, and this is the check that says so.
    NetworkReconciler::new(
        paths.network_dir.clone(),
        MockReload::new(),
        NoDelete,
        // No tunnel in this test's tree, so no key is ever drawn; the
        // directory is inside the same temporary tree either way.
        Keystore::under(dir.path(), None),
    )
    .apply(&Settings::default())
    .await
    .unwrap();

    assert!(
        paths.networkd().exists(),
        "the network reconciler swept the station's networkd unit away"
    );
    assert_eq!(
        std::fs::read_to_string(paths.networkd()).unwrap(),
        GOLDEN_NETWORKD
    );
}

#[tokio::test]
pub(super) async fn renaming_the_interface_removes_the_previous_networkd_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();

    let renamed = WifiClientSettings {
        interface: "wlan1".to_string(),
        ..one_psk_network()
    };
    reconciler.apply(&settings_with(renamed)).await.unwrap();

    assert!(
        !paths.networkd().exists(),
        "the old interface's unit would keep running DHCP on a link micad no longer manages"
    );
    assert!(
        paths
            .network_dir
            .join("90-wifi-client-wlan1.network")
            .exists()
    );
}

// ---- unit lifecycle and convergence -----------------------------------
