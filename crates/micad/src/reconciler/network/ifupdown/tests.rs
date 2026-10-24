use micad_settings::{BridgeConfig, CanConfig, RouteConfig};

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
    let text = render(
        &network,
        &["eth0".into(), "eth4".into()],
        None,
        &crate::board::Board::default(),
    )
    .unwrap();
    assert_eq!(
        text,
        "# Managed by micad from `network`; the device's network. Do not edit.\n\
auto lo\niface lo inet loopback\n\
\nauto eth0\niface eth0 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    metric 100\n    udhcpc_opts -b -S\n\
\nauto eth1\niface eth1 inet static\n    address 192.168.1.10\n    netmask 255.255.255.0\n    gateway 192.168.1.1\n    up mkdir -p /run/mica/resolv.d && printf 'nameserver %s\\n' 1.1.1.1 9.9.9.9 >/run/mica/resolv.d/eth1 && { if [ -x /usr/lib/mica/mica-resolv ]; then /usr/lib/mica/mica-resolv; else { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf; fi; }\n    down rm -f /run/mica/resolv.d/eth1 && { if [ -x /usr/lib/mica/mica-resolv ]; then /usr/lib/mica/mica-resolv; else { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf; fi; }\n\
\nauto eth2\niface eth2 inet manual\n\
\nauto eth3\niface eth3 inet6 static\n    address fd00::2\n    netmask 64\n    gateway fd00::1\n\
\nauto eth4\niface eth4 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    metric 110\n    udhcpc_opts -b -S\n"
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
async fn what_only_networkd_does_or_no_one_can_is_refused_by_name_and_nothing_is_run() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    let bridge = IfaceSettings {
        kind: IfaceKind::Bridge,
        bridge: Some(BridgeConfig::default()),
        ..IfaceSettings::default()
    };
    let misrouted = IfaceSettings {
        routes: vec![RouteConfig {
            destination: "nowhere".into(),
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
        (misrouted, "CIDR"),
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

/// Servers of its own on a DHCP interface: the lease is asked for no DNS, and
/// the servers sit in a resolver file that sorts before the lease's.
#[test]
fn a_dhcp_interface_with_its_own_dns_asks_the_lease_for_none() {
    let network = settings(&[(
        "eth0",
        IfaceSettings {
            dns: vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()],
            ..dhcp()
        },
    )])
    .network;
    let text = render(
        &network,
        &["eth0".into()],
        None,
        &crate::board::Board::default(),
    )
    .unwrap();
    assert_eq!(
        text,
        "# Managed by micad from `network`; the device's network. Do not edit.\n\
auto lo\niface lo inet loopback\n\
\nauto eth0\niface eth0 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    metric 100\n    udhcpc_opts -b -S -o -O subnet -O router -O broadcast\n    up mkdir -p /run/mica/resolv.d && printf 'nameserver %s\\n' 1.1.1.1 9.9.9.9 >/run/mica/resolv.d/00-eth0 && { if [ -x /usr/lib/mica/mica-resolv ]; then /usr/lib/mica/mica-resolv; else { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf; fi; }\n    down rm -f /run/mica/resolv.d/00-eth0 && { if [ -x /usr/lib/mica/mica-resolv ]; then /usr/lib/mica/mica-resolv; else { cat /run/mica/resolv.d/* 2>/dev/null || :; } >/run/mica/resolv.conf; fi; }\n"
    );
}

fn can(fd: Option<bool>, data_bitrate: Option<u32>, restart_ms: Option<u32>) -> IfaceSettings {
    IfaceSettings {
        kind: IfaceKind::Can,
        can: Some(CanConfig {
            bitrate: 250_000,
            restart_ms,
            fd,
            data_bitrate,
        }),
        ..IfaceSettings::default()
    }
}

/// A CAN controller is an unaddressed stanza whose `pre-up` sets its
/// parameters with iproute2's `ip`: the fields it declares, and CAN FD off
/// unless it switches it on, so an FD-capable controller comes up.
#[test]
fn a_can_interface_is_set_by_iproute2_before_it_comes_up() {
    let network = settings(&[
        ("can0", can(Some(false), None, Some(100))),
        ("can1", can(Some(true), Some(2_000_000), None)),
        ("can2", can(None, None, None)),
    ])
    .network;
    let text = render(
        &network,
        &[],
        Some(Path::new("/usr/sbin/ip")),
        &crate::board::Board::default(),
    )
    .unwrap();
    let stanza = |iface: &str, set: &str| {
        format!(
            "\nauto {iface}\niface {iface} inet manual\n    pre-up /usr/sbin/ip link set dev {iface} type can bitrate 250000{set}\n    up /usr/sbin/ip link set dev {iface} up\n    down /usr/sbin/ip link set dev {iface} down\n"
        )
    };
    assert_eq!(
        text,
        format!(
            "# Managed by micad from `network`; the device's network. Do not edit.\nauto lo\niface lo inet loopback\n{}{}{}",
            stanza("can0", " restart-ms 100 fd off"),
            stanza("can1", " fd on dbitrate 2000000"),
            stanza("can2", " fd off"),
        )
    );
}

/// A root without iproute2's `ip` refuses a CAN interface by name, and runs
/// and writes nothing; a CAN entry out of its rules is refused the same way.
#[tokio::test]
async fn a_can_interface_without_iproute2_or_out_of_its_rules_is_refused() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    let error = reconciler
        .apply(&settings(&[("can0", can(None, None, None))]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("needs iproute2's ip"), "{error}");
    let reconciler = reconciler.with_ip(Some(PathBuf::from("/usr/sbin/ip")));
    let error = reconciler
        .apply(&settings(&[("can0", can(Some(true), None, None))]))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("needs can.dataBitrate"),
        "{error}"
    );
    assert!(fake.calls().is_empty());
    assert!(!reconciler.path.exists());
}

/// With iproute2, the CAN stanza is written and brought up like any other.
#[tokio::test]
async fn a_can_interface_with_iproute2_is_written_and_brought_up() {
    let (_dir, fake, reconciler) = fixture(&[]);
    let reconciler = reconciler.with_ip(Some(PathBuf::from("/usr/sbin/ip")));
    let state = reconciler
        .apply(&settings(&[("can0", can(Some(false), None, Some(100)))]))
        .await
        .unwrap();
    assert_eq!(fake.calls(), ["ifdown -a", "ifup -a"]);
    let text = std::fs::read_to_string(&reconciler.path).unwrap();
    assert!(
        text.contains(
            "pre-up /usr/sbin/ip link set dev can0 type can bitrate 250000 restart-ms 100 fd off"
        ),
        "{text}"
    );
    assert_eq!(state["can0"]["kind"], "can");
}

/// iproute2's `ip` is the first present path, unless it is BusyBox's.
#[test]
fn iproute2_is_the_first_ip_present_and_not_busybox() {
    let dir = tempfile::tempdir().unwrap();
    let busybox = dir.path().join("busybox");
    let real = dir.path().join("iproute2-ip");
    std::fs::write(&busybox, "").unwrap();
    std::fs::write(&real, "").unwrap();
    let linked = dir.path().join("sbin-ip");
    std::os::unix::fs::symlink(&busybox, &linked).unwrap();
    let absent = dir.path().join("absent");
    assert_eq!(iproute2_ip(&[&absent, &real]), Some(real.clone()));
    assert_eq!(iproute2_ip(&[&linked, &real]), None);
    assert_eq!(iproute2_ip(&[&absent]), None);
}

/// Each DHCP link's default route gets its own metric, in name order across
/// declared and undeclared ports, as networkd's units give them; a static or
/// CAN link gets none.
#[test]
fn dhcp_links_get_distinct_route_metrics_in_name_order() {
    let network = settings(&[
        ("eth1", dhcp()),
        ("eth2", fixed("192.168.1.10/24", Some("192.168.1.1"), &[])),
    ])
    .network;
    let text = render(
        &network,
        &["eth0".into(), "eth3".into()],
        None,
        &crate::board::Board::default(),
    )
    .unwrap();
    let metrics: Vec<(&str, &str)> = text
        .split("\nauto ")
        .skip(2)
        .map(|stanza| {
            let iface = stanza.split('\n').next().unwrap();
            let metric = stanza
                .lines()
                .find_map(|line| line.trim().strip_prefix("metric "))
                .unwrap_or("-");
            (iface, metric)
        })
        .collect();
    assert_eq!(
        metrics,
        [
            ("eth1", "110"),
            ("eth2", "-"),
            ("eth0", "100"),
            ("eth3", "120")
        ]
    );
}

/// An interface kept off the internet hands Base's udhcpc script
/// `IF_INTERNET=0`, which then sets no default route from the lease.
#[test]
fn a_dhcp_interface_off_the_internet_says_so_to_the_lease_script() {
    let network = settings(&[(
        "eth0",
        IfaceSettings {
            internet: Some(false),
            ..dhcp()
        },
    )])
    .network;
    let text = render(&network, &[], None, &crate::board::Board::default()).unwrap();
    assert!(
        text.contains(
            "\nauto eth0\niface eth0 inet dhcp\n    script /usr/lib/mica/mica-udhcpc\n    metric 100\n    internet 0\n    udhcpc_opts -b -S\n"
        ),
        "{text}"
    );
}

/// A board's internal port reaches the lease script as off the internet,
/// declared or not, until its entry says otherwise.
#[test]
fn a_boards_internal_port_is_off_the_internet_until_its_entry_chooses() {
    let board = crate::board::Board::parse(
        r#"{"version":1,"interfaces":[
            {"name":"eth0","kind":"ethernet","label":"LAN1","role":"internal"},
            {"name":"eth1","kind":"ethernet","label":"LAN2","role":"internal"}]}"#,
    )
    .unwrap();
    let stanza = |text: &str, iface: &str| {
        text.split("\nauto ")
            .find(|stanza| stanza.starts_with(&format!("{iface}\n")))
            .unwrap()
            .to_string()
    };
    let network = settings(&[("eth0", dhcp())]).network;
    let text = render(&board.effective(&network), &["eth1".into()], None, &board).unwrap();
    assert!(stanza(&text, "eth0").contains("    internet 0\n"), "{text}");
    assert!(stanza(&text, "eth1").contains("    internet 0\n"), "{text}");

    let chosen = settings(&[(
        "eth0",
        IfaceSettings {
            internet: Some(true),
            ..dhcp()
        },
    )])
    .network;
    let text = render(&board.effective(&chosen), &[], None, &board).unwrap();
    assert!(!stanza(&text, "eth0").contains("internet"), "{text}");
}

/// Static routes ride their interface's stanza. On a static or manual
/// stanza `ip route` adds each once the link is up and removes it before it
/// goes down; on DHCP each is a `route_<n>` option, which Base's udhcpc script
/// reads as `IF_ROUTE_<n>` and applies once the lease is bound.
#[test]
fn static_routes_are_added_and_removed_with_their_interface() {
    let route = |destination: &str, gateway: Option<&str>, metric: Option<u32>| RouteConfig {
        destination: destination.into(),
        gateway: gateway.map(str::to_string),
        metric,
    };
    let network = settings(&[
        (
            "eth0",
            IfaceSettings {
                routes: vec![
                    route("10.20.0.0/16", Some("192.168.1.2"), Some(5)),
                    route("10.21.0.0/16", None, None),
                ],
                ..dhcp()
            },
        ),
        (
            "eth1",
            IfaceSettings {
                routes: vec![route("10.30.0.0/16", None, None)],
                ..fixed("192.168.10.1/24", None, &[])
            },
        ),
    ])
    .network;
    let text = render(&network, &[], None, &crate::board::Board::default()).unwrap();
    assert!(
        text.contains(
            "    udhcpc_opts -b -S\n    route_1 10.20.0.0/16 via 192.168.1.2 metric 5\n    route_2 10.21.0.0/16\n"
        ),
        "{text}"
    );
    assert!(!text.contains("dev eth0"), "{text}");
    assert!(
        text.contains(
            "    netmask 255.255.255.0\n    up ip route add 10.30.0.0/16 dev eth1\n    down ip route del 10.30.0.0/16 dev eth1 || :\n"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn an_interface_with_static_routes_is_applied() {
    let (_dir, fake, reconciler) = fixture(&["eth0"]);
    let routed = IfaceSettings {
        routes: vec![RouteConfig {
            destination: "10.1.0.0/16".into(),
            gateway: Some("192.168.1.2".into()),
            metric: None,
        }],
        ..dhcp()
    };
    reconciler
        .apply(&settings(&[("eth0", routed)]))
        .await
        .unwrap();
    assert!(!fake.calls().is_empty());
}
