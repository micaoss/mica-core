use super::*;
use crate::network_state::adopt::{adopted_entry, adoption};

/// Base's `80-dhcp.network`.
const IMAGE_DHCP: &str = "[Match]\nName=eth*\n\n[Network]\nDHCP=yes\n";

#[test]
fn the_image_dhcp_file_is_adopted_as_a_dhcp_entry() {
    assert_eq!(adopted_entry(&[IMAGE_DHCP]), Ok(json!({ "dhcp": true })));
}

/// The unit micad renders for an undeclared wired port: the image's DHCP
/// with a route metric, which is micad's to render again once declared.
#[test]
fn the_auto_dhcp_unit_is_adopted_as_a_dhcp_entry() {
    let unit = "[Match]\nName=eth1\n\n[Network]\nDHCP=yes\nLinkLocalAddressing=yes\n\n[DHCPv4]\nRouteMetric=110\n";
    assert_eq!(adopted_entry(&[unit]), Ok(json!({ "dhcp": true })));
    assert_eq!(
        adopted_entry(&["[Network]\nDHCP=yes\n[DHCPv4]\nUseDNS=no\n"]),
        Err("[DHCPv4] UseDNS=".to_string())
    );
}

#[test]
fn a_static_file_is_adopted_with_its_address_gateway_and_servers() {
    let unit = "# a comment\n[Match]\nName=eth1\nMACAddress=02:00:00:00:00:01\n\n\
        [Network]\nAddress=192.168.10.2/24\nGateway=192.168.10.1\nDNS=1.1.1.1 9.9.9.9\n";
    assert_eq!(
        adopted_entry(&[unit]),
        Ok(json!({
            "dhcp": false,
            "static": { "address": "192.168.10.2/24", "gateway": "192.168.10.1", "dns": ["1.1.1.1", "9.9.9.9"] },
        }))
    );
}

#[test]
fn a_drop_in_counts_as_part_of_the_file() {
    let drop_in = "[Network]\nDHCP=no\nAddress=10.0.0.2/24\n";
    assert_eq!(
        adopted_entry(&[IMAGE_DHCP, drop_in]),
        Ok(json!({ "dhcp": false, "static": { "address": "10.0.0.2/24", "dns": [] } }))
    );
}

#[test]
fn what_an_entry_cannot_say_is_named() {
    for (unit, refused) in [
        (
            "[Network]\nDHCP=yes\n[Route]\nGateway=10.0.0.1\n",
            "its [Route] section",
        ),
        ("[Network]\nDHCP=yes\nBond=bond0\n", "[Network] Bond="),
        ("[Network]\nDHCP=ipv6\n", "DHCP=ipv6"),
        (
            "[Network]\nAddress=10.0.0.2/24\nAddress=10.0.1.2/24\n",
            "more than one Address=",
        ),
        ("[Network]\nDHCP=yes\nDNS=1.1.1.1\n", "DNS= beside DHCP="),
        (
            "[Network]\nDHCP=yes\nAddress=10.0.0.2/24\n",
            "Address= beside DHCP=",
        ),
    ] {
        assert_eq!(adopted_entry(&[unit]), Err(refused.to_string()), "{unit}");
    }
}

#[test]
fn a_link_on_its_image_file_is_adoptable_and_one_on_micads_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("80-dhcp.network");
    std::fs::write(&file, IMAGE_DHCP).unwrap();
    let mica_dns = dir.path().join("90-mica-global-dns.conf");
    std::fs::write(&mica_dns, "[DHCPv4]\nUseDNS=no\n").unwrap();
    let file = file.to_str().unwrap();

    let link = json!({
        "Name": "eth1",
        "NetworkFile": file,
        "NetworkFileDropins": [mica_dns.to_str().unwrap()],
    });
    assert_eq!(
        adoption(&link),
        Some(json!({ "networkFile": file, "entry": { "dhcp": true } }))
    );

    let ours =
        json!({ "Name": "eth0", "NetworkFile": "/run/systemd/network/50-mica-eth0.network" });
    assert_eq!(adoption(&ours), None);
    assert_eq!(adoption(&json!({ "Name": "lo" })), None);
}
