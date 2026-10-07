//! The declaration rules: kinds, blocks, parents and ports.

use micad_settings::BridgeConfig;
use micad_settings::IfaceSettings;

use super::*;

#[tokio::test]
pub(super) async fn rejects_a_kind_whose_own_block_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[(
        "eth0.100",
        IfaceSettings {
            kind: IfaceKind::Vlan,
            dhcp: true,
            ..IfaceSettings::default()
        },
    )]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(err.to_string().contains("carries no vlan block"), "{err}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
pub(super) async fn rejects_a_block_that_does_not_match_the_kind() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    // A physical entry with bridge parameters is a statement about the
    // link that the render would silently drop.
    let settings = settings_with(&[
        (
            "eth0",
            IfaceSettings {
                dhcp: true,
                bridge: Some(BridgeConfig::default()),
                ..IfaceSettings::default()
            },
        ),
        ("eth1", port_iface()),
    ]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(
        err.to_string()
            .contains("is kind physical but carries a bridge block"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rejects_a_vlan_parent_that_is_not_declared() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("eth0.100", vlan_iface("eth9", 100))]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    // Fail closed: the VLAN line lives in the parent's unit, so an
    // undeclared parent is a VLAN that would never come up.
    assert!(
        err.to_string()
            .contains("VLAN parent \"eth9\", which is not a declared network entry"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rejects_a_bridge_port_that_is_not_declared() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("br0", bridge_iface(&["eth9"]))]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(
        err.to_string()
            .contains("bridge port \"eth9\", which is not a declared network entry"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rejects_a_bridge_port_that_carries_its_own_addressing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("br0", bridge_iface(&["eth1"])), ("eth1", static_iface())]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    // The port's `.network` is `Bridge=br0` and nothing else, so an
    // address on it is a value the render has no line for.
    assert!(
        err.to_string()
            .contains("must not carry addressing of its own"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rejects_a_port_two_bridges_both_claim() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("br0", bridge_iface(&["eth1"])),
        ("br1", bridge_iface(&["eth1"])),
        ("eth1", port_iface()),
    ]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    // One port, one `Bridge=` line: the second claim has to be an error
    // rather than whichever bridge the map iteration reached last.
    assert!(
        err.to_string()
            .contains("claimed as a port by both bridge br0 and bridge br1"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// `dns` replaces a lease's servers, so it belongs to a DHCP entry only, and
/// each server is an address.
#[tokio::test]
pub(super) async fn rejects_dns_of_its_own_on_an_entry_that_is_not_a_dhcp_client() {
    for (cfg, reason) in [
        (
            IfaceSettings {
                dns: vec!["1.1.1.1".to_string()],
                ..static_iface()
            },
            "a static entry names them in `static.dns`",
        ),
        (
            IfaceSettings {
                dns: vec!["1.1.1.1".to_string()],
                ..IfaceSettings::default()
            },
            "a static entry names them in `static.dns`",
        ),
        (
            IfaceSettings {
                dns: vec!["dns.example".to_string()],
                ..dhcp_iface()
            },
            "is not an IP address",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (reconciler, calls) = reconciler_in(dir.path());
        let err = reconciler
            .apply(&settings_with(&[("eth0", cfg)]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(reason), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(calls.lock().unwrap().is_empty());
    }
}
