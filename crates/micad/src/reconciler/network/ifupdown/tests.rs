use micad_settings::{BridgeConfig, RouteConfig};

use super::*;
use crate::openrc::FakeCommands;

fn dhcp() -> IfaceSettings {
    IfaceSettings {
        dhcp: true,
        ..IfaceSettings::default()
    }
}

fn fixed(address: &str, gateway: Option<&str>, dns: &[&str]) -> IfaceSettings {
    IfaceSettings {
        static_: Some(StaticConfig {
            address: address.to_string(),
            gateway: gateway.map(str::to_string),
            dns: dns.iter().map(|d| (*d).to_string()).collect(),
        }),
        ..IfaceSettings::default()
    }
}

fn settings(network: &[(&str, IfaceSettings)]) -> Settings {
    Settings {
        network: network
            .iter()
            .map(|(name, cfg)| ((*name).to_string(), cfg.clone()))
            .collect(),
        ..Settings::default()
    }
}

fn fixture(wired: &[&str]) -> (tempfile::TempDir, Arc<FakeCommands>, IfupdownReconciler) {
    let dir = tempfile::tempdir().unwrap();
    let net = dir.path().join("net");
    for name in wired {
        std::fs::create_dir_all(net.join(name)).unwrap();
    }
    std::fs::create_dir_all(net.join("lo")).unwrap();
    let fake = Arc::new(FakeCommands::default());
    let reconciler = IfupdownReconciler::new(
        dir.path().join("network/interfaces"),
        net,
        Arc::clone(&fake) as Arc<dyn Commands>,
    );
    (dir, fake, reconciler)
}

#[test]
fn dhcp_static_and_unaddressed_interfaces_render_as_ifupdown_stanzas() {
    let network = settings(&[
        ("eth0", dhcp()),
        (
            "eth1",
            fixed(
                "192.168.1.10/24",
                Some("192.168.1.1"),
                &["1.1.1.1", "9.9.9.9"],
            ),
        ),
        ("eth2", IfaceSettings::default()),
        ("eth3", fixed("fd00::2/64", Some("fd00::1"), &[])),
    ])
    .network;
    let text = render(&network, &["eth0".into(), "eth4".into()]).unwrap();
    assert_eq!(
        text,
        "# Managed by micad from `network`; the device's network. Do not edit.\n\
auto lo\niface lo inet loopback\n\
\nauto eth0\niface eth0 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    udhcpc_opts -b -S\n\
\nauto eth1\niface eth1 inet static\n    address 192.168.1.10\n    netmask 255.255.255.0\n    gateway 192.168.1.1\n    up mkdir -p /run/mica/resolv.d && printf 'nameserver %s\\n' 1.1.1.1 9.9.9.9 >/run/mica/resolv.d/eth1 && { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf\n    down rm -f /run/mica/resolv.d/eth1 && { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf\n\
\nauto eth2\niface eth2 inet manual\n\
\nauto eth3\niface eth3 inet6 static\n    address fd00::2\n    netmask 64\n    gateway fd00::1\n\
\nauto eth4\niface eth4 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    udhcpc_opts -b -S\n"
    );
}

#[test]
fn an_address_without_a_prefix_is_a_host_address() {
    assert_eq!(netmask_v4(32), "255.255.255.255");
    assert_eq!(netmask_v4(0), "0.0.0.0");
    assert_eq!(address_and_prefix("10.0.0.1").unwrap().1, 32);
}

#[tokio::test]
async fn a_changed_file_goes_down_on_the_old_file_and_up_on_the_new() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    let state = reconciler
        .apply(&settings(&[("eth0", dhcp())]))
        .await
        .unwrap();
    assert_eq!(fake.calls(), ["ifdown -a", "ifup -a"]);
    assert_eq!(state["eth0"]["dhcp"], true);
    // The same settings again change nothing and run nothing.
    reconciler
        .apply(&settings(&[("eth0", dhcp())]))
        .await
        .unwrap();
    assert_eq!(fake.calls().len(), 2);
    let text = std::fs::read_to_string(&reconciler.path).unwrap();
    assert!(text.contains("iface eth0 inet dhcp"));
}

#[tokio::test]
async fn a_failed_ifup_is_an_error() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    fake.answer("ifup -a", 1, "");
    assert!(
        reconciler
            .apply(&settings(&[("eth0", dhcp())]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn what_only_networkd_does_is_refused_by_name_and_nothing_is_run() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    let bridge = IfaceSettings {
        kind: IfaceKind::Bridge,
        bridge: Some(BridgeConfig::default()),
        ..IfaceSettings::default()
    };
    let routed = IfaceSettings {
        routes: vec![RouteConfig {
            destination: "10.1.0.0/16".into(),
            gateway: None,
            metric: None,
        }],
        ..dhcp()
    };
    let both = IfaceSettings {
        dhcp: true,
        ..fixed("192.168.1.10/24", None, &[])
    };
    let mixed = fixed("192.168.1.10/24", Some("fd00::1"), &[]);
    for (cfg, word) in [
        (bridge, "bridge"),
        (routed, "routes"),
        (both, "DHCP and a static address"),
        (mixed, "family"),
    ] {
        let error = reconciler
            .apply(&settings(&[("br0", cfg)]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(word), "{error}");
    }
    assert!(fake.calls().is_empty());
}
