//! Rendering DHCP and static interfaces, and sweeping stale files.

use micad_settings::IfaceSettings;

use super::*;

#[tokio::test]
pub(super) async fn renders_dhcp_iface() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("eth0", dhcp_iface())]);

    reconciler.apply(&settings).await.unwrap();

    let rendered = std::fs::read_to_string(dir.path().join("50-mica-eth0.network")).unwrap();
    assert_eq!(rendered, GOLDEN_DHCP);
}

#[tokio::test]
pub(super) async fn renders_static_iface() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("eth1", static_iface())]);

    reconciler.apply(&settings).await.unwrap();

    let rendered = std::fs::read_to_string(dir.path().join("50-mica-eth1.network")).unwrap();
    assert_eq!(rendered, GOLDEN_STATIC);
}

#[tokio::test]
pub(super) async fn renders_static_less_non_dhcp_iface_with_empty_network_section() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[(
        "eth2",
        IfaceSettings {
            dhcp: false,
            ..IfaceSettings::default()
        },
    )]);

    reconciler.apply(&settings).await.unwrap();

    let rendered = std::fs::read_to_string(dir.path().join("50-mica-eth2.network")).unwrap();
    assert_eq!(rendered, GOLDEN_EMPTY);
}

#[tokio::test]
pub(super) async fn renders_both_ifaces_and_returns_live_state() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("eth0", dhcp_iface()), ("eth1", static_iface())]);

    let state = reconciler.apply(&settings).await.unwrap();

    let dhcp = std::fs::read_to_string(dir.path().join("50-mica-eth0.network")).unwrap();
    let static_ = std::fs::read_to_string(dir.path().join("50-mica-eth1.network")).unwrap();
    assert_eq!(dhcp, GOLDEN_DHCP);
    assert_eq!(static_, GOLDEN_STATIC);
    assert_eq!(
        state,
        json!({
            "eth0": { "file": "50-mica-eth0.network", "dhcp": true, "kind": "physical" },
            "eth1": { "file": "50-mica-eth1.network", "dhcp": false, "kind": "physical" },
        })
    );
    assert_eq!(*calls.lock().unwrap(), vec!["reload".to_string()]);
    assert_eq!(reconciler.name(), "network");
    assert_eq!(reconciler.subtree(), "network");
}

#[tokio::test]
pub(super) async fn removes_stale_mica_managed_files_but_keeps_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    std::fs::write(dir.path().join("50-mica-eth9.network"), "stale").unwrap();
    std::fs::write(dir.path().join("80-dhcp.network"), "foreign").unwrap();
    let settings = settings_with(&[("eth0", dhcp_iface())]);

    reconciler.apply(&settings).await.unwrap();

    assert!(!dir.path().join("50-mica-eth9.network").exists());
    assert!(dir.path().join("80-dhcp.network").exists());
    assert!(dir.path().join("50-mica-eth0.network").exists());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
pub(super) async fn rejects_an_iface_name_that_would_escape_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("../evil", dhcp_iface())]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(err.to_string().contains("interface"), "{err}");
    // Nothing was rendered and networkd was never told to reload: the
    // reconcile aborted before any I/O it would have to undo.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
pub(super) async fn rejects_a_gateway_that_is_not_an_address() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let mut cfg = static_iface();
    // A newline here would land verbatim on the Gateway= line, where it
    // starts a new networkd directive.
    cfg.static_.as_mut().unwrap().gateway = Some("192.168.1.1\nDNS=6.6.6.6".to_string());
    let settings = settings_with(&[("eth1", cfg)]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(err.to_string().contains("gateway"), "{err}");
    assert!(!dir.path().join("50-mica-eth1.network").exists());
}

#[tokio::test]
pub(super) async fn sweep_spares_a_wifi_unit_whose_iface_embeds_mica() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    // The wifi reconcilers embed the interface in their unit names, and
    // `a-mica-b` is a legal interface name; the sweep must only ever eat
    // its own `50-mica-*` namespace.
    std::fs::write(dir.path().join("90-wifi-client-a-mica-b.network"), "wifi").unwrap();
    let settings = settings_with(&[("eth0", dhcp_iface())]);

    reconciler.apply(&settings).await.unwrap();

    assert!(dir.path().join("90-wifi-client-a-mica-b.network").exists());
}

/// A DHCP entry's own servers replace the lease's in every way a lease or a
/// router advertisement can name one.
#[tokio::test]
pub(super) async fn a_dhcp_iface_with_its_own_dns_ignores_the_leases() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[(
        "eth0",
        IfaceSettings {
            dns: vec!["1.1.1.1".to_string(), "2606:4700:4700::1111".to_string()],
            ..dhcp_iface()
        },
    )]);

    reconciler.apply(&settings).await.unwrap();

    let rendered = std::fs::read_to_string(dir.path().join("50-mica-eth0.network")).unwrap();
    assert_eq!(
        rendered,
        "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\nDNS=1.1.1.1\nDNS=2606:4700:4700::1111\n\n\
[DHCPv4]\nUseDNS=no\n\n[DHCPv6]\nUseDNS=no\n\n[IPv6AcceptRA]\nUseDNS=no\n"
    );
}
