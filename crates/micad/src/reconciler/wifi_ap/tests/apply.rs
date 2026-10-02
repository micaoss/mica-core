//! Writing the configuration and the networkd unit.

use super::super::super::network::{NetworkReconciler, NoDelete};
use crate::wgkeys::Keystore;
use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
pub(super) async fn apply_writes_the_golden_config_at_0600_creating_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(&paths.config).unwrap(),
        GOLDEN_CONFIG
    );
    assert_eq!(
        mode_of(&paths.config),
        0o600,
        "the file carries the pre-shared key and must not be readable by anyone else"
    );
    assert_eq!(state["accessPoint"], json!("applied"));
    assert_eq!(state["config"], json!(paths.config.display().to_string()));
    assert_eq!(state["unit"], json!("hostapd@wlan0.service"));
    assert_eq!(reconciler.name(), "wifiAp");
    assert_eq!(reconciler.subtree(), "wifi");
}

#[tokio::test]
pub(super) async fn a_leftover_temporary_file_does_not_widen_the_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    // The shape an interrupted run leaves behind: the temporary name
    // already exists, so the mode given at open time is never applied.
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let leftover = paths.config_dir.join(".wlan0.conf.micad-tmp");
    std::fs::write(&leftover, "stale").unwrap();
    std::fs::set_permissions(&leftover, std::fs::Permissions::from_mode(0o666)).unwrap();

    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

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

    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

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
pub(super) async fn a_running_access_point_gets_an_address_and_a_dhcp_server() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    assert_eq!(
        std::fs::read_to_string(paths.networkd()).unwrap(),
        GOLDEN_NETWORKD,
        "an access point that hands out no address is one nothing can reach"
    );
    assert_eq!(state["networkdUnit"], json!("90-wifi-ap-wlan0.network"));
    assert_eq!(reconciler.reloader.calls(), vec!["reload".to_string()]);
}

// ---- surviving the network reconciler's sweep -------------------------

#[tokio::test]
pub(super) async fn the_rendered_networkd_unit_survives_the_network_reconcilers_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler.apply(&settings_with(lab_ap())).await.unwrap();
    assert!(paths.networkd().exists());

    // The network reconciler deletes every `*-mica-*.network` it did not
    // itself render. Sharing a directory with it means this reconciler's
    // unit has to be outside that pattern, and running the real thing over
    // the same directory is the only check that says so.
    NetworkReconciler::new(
        paths.network_dir.clone(),
        MockReload::new(),
        NoDelete,
        // No tunnel in this test's tree, so no key is ever drawn; the
        // directory is inside the same temporary tree either way.
        Keystore::under(&paths.state_dir, None),
    )
    .apply(&Settings::default())
    .await
    .unwrap();

    assert!(
        paths.networkd().exists(),
        "the network reconciler swept the access point's networkd unit away"
    );
    assert_eq!(
        std::fs::read_to_string(paths.networkd()).unwrap(),
        GOLDEN_NETWORKD
    );
}

#[tokio::test]
pub(super) async fn the_networkd_unit_sorts_after_the_units_it_must_not_override() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    let name = networkd_file_name("wlan0");
    for earlier in ["50-mica-wlan0.network", "80-dhcp.network"] {
        assert!(
            name.as_str() > earlier,
            "networkd applies the first matching unit in lexical order, so \
             {name} must sort after {earlier}"
        );
    }
    assert!(paths.networkd().exists());
}

#[tokio::test]
pub(super) async fn renaming_the_interface_removes_the_previous_networkd_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    let renamed = WifiApSettings {
        interface: "wlan1".to_string(),
        ..lab_ap()
    };
    reconciler.apply(&settings_with(renamed)).await.unwrap();

    assert!(
        !paths.networkd().exists(),
        "the old interface would keep an AP address and a DHCP server on a \
         link micad no longer manages"
    );
    assert!(paths.network_dir.join("90-wifi-ap-wlan1.network").exists());
}

// ---- unit lifecycle and convergence -----------------------------------
