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
            render_networkd("wlan0", address, None, None, &[]).unwrap(),
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
            render_networkd("wlan0", bad, None, None, &[]).is_err(),
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

#[test]
pub(super) fn a_declared_pool_and_lease_replace_the_derived_ones() {
    let pool = micad_settings::ApPool {
        offset: 100,
        size: 50,
    };
    assert_eq!(
        render_networkd("wlan0", "192.168.4.1/24", Some(pool), Some(3600), &[]).unwrap(),
        "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.4.1/24\nDHCPServer=yes\n\n\
         [DHCPServer]\nPoolOffset=100\nPoolSize=50\nDefaultLeaseTimeSec=3600\n"
    );
    // The access point's own address is never handed out.
    let own = micad_settings::ApPool {
        offset: 1,
        size: 10,
    };
    assert!(render_networkd("wlan0", "192.168.4.1/24", Some(own), None, &[]).is_err());
    let past = micad_settings::ApPool {
        offset: 200,
        size: 56,
    };
    assert!(render_networkd("wlan0", "192.168.4.1/24", Some(past), None, &[]).is_err());
}

// With global DNS on, the clients are handed the global servers: the uplinks
// learn none to pass on. Off, networkd hands out the uplink's own.
#[test]
pub(super) fn global_dns_servers_are_what_the_clients_are_handed() {
    let servers = ["9.9.9.9".to_string(), "1.1.1.1".to_string()];
    assert_eq!(
        render_networkd("wlan0", "192.168.4.1/24", None, None, &servers).unwrap(),
        "[Match]\nName=wlan0\n\n[Network]\nAddress=192.168.4.1/24\nDHCPServer=yes\n\n\
         [DHCPServer]\nPoolOffset=2\nPoolSize=253\nDNS=9.9.9.9 1.1.1.1\n"
    ); // A server that is not an address never reaches the file.
    let injected = ["9.9.9.9\n[Network]\nDHCP=yes".to_string()];
    assert!(render_networkd("wlan0", "192.168.4.1/24", None, None, &injected).is_err());
}

#[tokio::test]
pub(super) async fn a_global_dns_change_rewrites_the_access_point_unit() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    assert!(reconciler.reads().contains(&"dns"));
    let mut settings = settings_with(lab_ap());
    reconciler.apply(&settings).await.unwrap();
    let unit = || std::fs::read_to_string(paths.networkd()).unwrap();
    assert!(!unit().contains("DNS="), "{}", unit());

    settings.dns.global = micad_settings::GlobalDns {
        enabled: true,
        servers: vec!["9.9.9.9".to_string()],
    };
    reconciler.apply(&settings).await.unwrap();
    assert!(unit().ends_with("DNS=9.9.9.9\n"), "{}", unit());
    // Switched off, the list kept is not handed out.
    settings.dns.global.enabled = false;
    reconciler.apply(&settings).await.unwrap();
    assert!(!unit().contains("DNS="), "{}", unit());
}
