//! The default-route reads and the connection interval.

use super::super::bluez::*;

#[test]
fn a_default_route_is_read_from_either_table() {
    let v4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
              eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
              eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
    assert!(ipv4_default(v4));
    let ap_only = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                   wlan0\t0004A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
    assert!(!ipv4_default(ap_only));

    let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n\
              00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
    assert!(ipv6_default(v6));
    let unreachable_only = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
    assert!(!ipv6_default(unreachable_only));
}

#[test]
fn the_connection_interval_moves_in_order_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| dir.path().join(name);
    std::fs::write(file("conn_min_interval"), "24\n").unwrap();
    std::fs::write(file("conn_max_interval"), "40\n").unwrap();
    let before = set_interval(dir.path(), (9, 12)).unwrap();
    assert_eq!(before, (24, 40));
    let read = |name: &str| std::fs::read_to_string(file(name)).unwrap();
    assert_eq!(
        (read("conn_min_interval"), read("conn_max_interval")),
        ("9\n".into(), "12\n".into())
    );
    assert_eq!(set_interval(dir.path(), before).unwrap(), (9, 12));
    assert_eq!(
        (read("conn_min_interval"), read("conn_max_interval")),
        ("24\n".into(), "40\n".into())
    );
    assert!(set_interval(&dir.path().join("hci9"), (9, 12)).is_err());
}
