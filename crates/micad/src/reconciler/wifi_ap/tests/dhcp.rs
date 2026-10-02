//! The DHCP pool derived from the access point address.

use super::*;

#[test]
pub(super) fn the_dhcp_pool_is_derived_from_the_access_point_address() {
    for (address, expected) in [
        (
            "192.168.4.1/24",
            "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.4.1/24\nDHCPServer=yes\n\n\
             [DHCPServer]\nPoolOffset=2\nPoolSize=253\n",
        ),
        (
            "10.7.0.1/16",
            "[Match]\nName=wlan0\n\n[Network]\nAddress=10.7.0.1/16\nDHCPServer=yes\n\n\
             [DHCPServer]\nPoolOffset=2\nPoolSize=65533\n",
        ),
        (
            "192.168.9.129/25",
            "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.9.129/25\nDHCPServer=yes\n\n\
             [DHCPServer]\nPoolOffset=2\nPoolSize=125\n",
        ),
        (
            "192.168.4.200/24",
            "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.4.200/24\nDHCPServer=yes\n\n\
             [DHCPServer]\nPoolOffset=201\nPoolSize=54\n",
        ),
        (
            "192.168.4.253/30",
            "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.4.253/30\nDHCPServer=yes\n\n\
             [DHCPServer]\nPoolOffset=2\nPoolSize=1\n",
        ),
    ] {
        assert_eq!(
            render_networkd("wlan0", address).unwrap(),
            expected,
            "pool for {address}"
        );
    }
}

#[test]
pub(super) fn the_pool_always_lies_inside_the_subnet_and_excludes_the_host() {
    for (address, host, last) in [
        ("192.168.4.1/24", 1u32, 255u32),
        ("192.168.9.129/25", 1, 127),
        ("192.168.4.200/24", 200, 255),
    ] {
        let (ip, prefix) = parse_cidr(address).unwrap();
        let (offset, size) = dhcp_pool(ip, prefix).unwrap();
        assert!(
            offset > host,
            "the pool would hand out the AP's own address"
        );
        assert!(
            offset + size <= last,
            "the pool runs past the broadcast address of {address}"
        );
        assert!(size > 0, "no client could get an address on {address}");
    }
}

#[test]
pub(super) fn an_address_that_cannot_carry_a_pool_is_rejected() {
    for bad in [
        "192.168.4.1",            // no prefix at all
        "192.168.4.1/",           // no prefix length
        "192.168.4.1/33",         // not a v4 prefix
        "192.168.4.1/31",         // no room for a pool
        "192.168.4.1/32",         // no room for a pool
        "192.168.4.0/24",         // the subnet address
        "192.168.4.255/24",       // the broadcast address
        "192.168.4.254/24",       // leaves nothing to hand out
        "not-an-address/24",      // not an address
        "192.168.4.1/24\nEvil=1", // an injection attempt
    ] {
        assert!(
            render_networkd("wlan0", bad).is_err(),
            "{bad:?} was accepted as an access-point address"
        );
    }
}

#[tokio::test]
pub(super) async fn an_unusable_address_stops_the_apply_before_the_unit_is_started() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        address: "192.168.4.255/24".to_string(),
        ..lab_ap()
    };

    let err = reconciler.apply(&settings_with(ap)).await.unwrap_err();

    assert!(format!("{err:#}").contains("broadcast"), "{err:#}");
    assert!(
        reconciler.control.calls().is_empty(),
        "hostapd was started against an address that cannot serve clients"
    );
}

// ---- what apply writes ------------------------------------------------
