//! VLAN and bridge devices.

use super::*;

#[tokio::test]
pub(super) async fn renders_a_vlan_netdev_and_names_the_child_in_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.netdev")).unwrap();
    let parent = std::fs::read_to_string(dir.path().join("50-mica-eth0.network")).unwrap();
    let child = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.network")).unwrap();
    assert_eq!(netdev, GOLDEN_VLAN_NETDEV);
    // networkd creates the VLAN only because the parent's unit names it.
    assert_eq!(parent, GOLDEN_VLAN_PARENT);
    assert_eq!(child, GOLDEN_VLAN_CHILD);
    // A device that did not exist before is not deleted on the way in.
    assert_eq!(*calls.lock().unwrap(), vec!["reload".to_string()]);
}

#[tokio::test]
pub(super) async fn renders_a_bridge_netdev_and_gives_its_port_only_the_master() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("br0", bridge_iface(&["eth1"])), ("eth1", port_iface())]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-br0.netdev")).unwrap();
    let port = std::fs::read_to_string(dir.path().join("50-mica-eth1.network")).unwrap();
    let bridge = std::fs::read_to_string(dir.path().join("50-mica-br0.network")).unwrap();
    assert_eq!(netdev, GOLDEN_BRIDGE_NETDEV);
    assert_eq!(port, GOLDEN_BRIDGE_PORT);
    // The bridge itself carries the addressing its ports gave up.
    assert_eq!(bridge, "[Match]\nName=br0\n\n[Network]\nDHCP=yes\n");
    // A bridge has no netdev-less port file left behind and no VLAN line.
    assert!(!dir.path().join("50-mica-eth1.netdev").exists());
}

#[tokio::test]
pub(super) async fn deletes_the_kernel_device_of_a_removed_vlan() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let with_vlan = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);
    reconciler.apply(&with_vlan).await.unwrap();

    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();

    assert!(!dir.path().join("50-mica-eth0.100.netdev").exists());
    assert!(!dir.path().join("50-mica-eth0.100.network").exists());
    // The device is deleted BEFORE the reload, and the swept `.network`
    // asks for no deletion of its own: networkd would otherwise leave the
    // VLAN passing traffic until the next boot.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del eth0.100".to_string(),
            "reload".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn recreates_a_vlan_whose_id_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 100)),
        ]))
        .await
        .unwrap();

    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 200)),
        ]))
        .await
        .unwrap();

    // Netdev properties are applied when the device is created, so a
    // rewritten file alone would leave the old id in the kernel.
    let netdev = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.netdev")).unwrap();
    assert!(netdev.contains("Id=200"), "{netdev}");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del eth0.100".to_string(),
            "reload".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn reapplying_an_unchanged_vlan_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);
    reconciler.apply(&settings).await.unwrap();

    reconciler.apply(&settings).await.unwrap();

    // A convergent reconcile that tore its own VLAN down every pass would
    // drop the link on every settings write in the tree.
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["reload".to_string(), "reload".to_string()]
    );
}

#[tokio::test]
pub(super) async fn sweeps_a_stale_netdev_but_spares_a_foreign_one() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    std::fs::write(dir.path().join("50-mica-br9.netdev"), "stale").unwrap();
    std::fs::write(dir.path().join("70-vpn.netdev"), "foreign").unwrap();

    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();

    assert!(!dir.path().join("50-mica-br9.netdev").exists());
    assert!(dir.path().join("70-vpn.netdev").exists());
}
