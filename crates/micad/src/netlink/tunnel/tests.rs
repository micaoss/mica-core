use super::*;

/// The bytes `text` spells in hex, whitespace ignored.
fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

const WG0: &str = "77673000";

#[test]
fn a_new_link_is_a_wireguard_kind_that_may_not_exist_yet() {
    assert_eq!(
        new_link("wg0"),
        hex(&format!(
            "3c000000 1000 0506 01000000 00000000
             00000000 00000000 00000000 00000000
             0800 0300 {WG0}
             1400 1280 0d00 0100 776972656775617264 000000"
        ))
    );
}

#[test]
fn delete_mark_and_up_name_the_link_by_index() {
    assert_eq!(
        delete_link(7),
        hex("20000000 1100 0500 01000000 00000000
             00000000 07000000 00000000 00000000")
    );
    assert_eq!(
        mark_link(7),
        hex("28000000 1000 0500 01000000 00000000
             00000000 07000000 00000000 00000000
             0800 1400 6d696361")
    );
    assert_eq!(
        link_up(7),
        hex("20000000 1000 0500 01000000 00000000
             00000000 07000000 01000000 01000000")
    );
}

#[test]
fn an_address_is_added_in_place_and_deleted_by_value() {
    let address = ("10.8.0.1".parse().unwrap(), 24);
    let body = "0218 0000 07000000 0800 0200 0a080001 0800 0100 0a080001";
    assert_eq!(
        address_request(RTM_NEWADDR, 7, address),
        hex(&format!("28000000 1400 0505 01000000 00000000 {body}"))
    );
    assert_eq!(
        address_request(RTM_DELADDR, 7, address),
        hex(&format!("28000000 1500 0500 01000000 00000000 {body}"))
    );
}

#[test]
fn a_tunnel_route_is_micads_on_the_link_in_the_main_table() {
    let route = TunnelRoute {
        destination: ("10.20.0.0".parse().unwrap(), 24),
        gateway: None,
        source: Some("10.8.0.1".parse().unwrap()),
        metric: 0,
    };
    let body = "0218 0000 fe6d fd01 00000000
                0800 0100 0a140000
                0800 0400 07000000
                0800 0700 0a080001
                0800 0600 00000000";
    assert_eq!(
        route_request(RTM_NEWROUTE, 7, &route),
        hex(&format!("3c000000 1800 0506 01000000 00000000 {body}"))
    );
    assert_eq!(
        route_request(RTM_DELROUTE, 7, &route),
        hex(&format!("3c000000 1900 0500 01000000 00000000 {body}"))
    );
}

#[test]
fn a_route_through_a_gateway_has_universe_scope() {
    let route = TunnelRoute {
        destination: ("fd00:30::".parse().unwrap(), 64),
        gateway: Some("fd00:8::2".parse().unwrap()),
        source: None,
        metric: 1024,
    };
    let fd00_30 = "fd000030000000000000000000000000";
    let fd00_8_2 = "fd000008000000000000000000000002";
    assert_eq!(
        route_request(RTM_NEWROUTE, 7, &route),
        hex(&format!(
            "54000000 1800 0506 01000000 00000000
             0a40 0000 fe6d 0001 00000000
             1400 0100 {fd00_30}
             0800 0400 07000000
             1400 0500 {fd00_8_2}
             0800 0600 00040000"
        ))
    );
}

#[test]
fn set_device_removes_named_peers_and_sets_the_others_whole() {
    let device = DeviceSet {
        private_key: [1; 32],
        listen_port: Some(51820),
        peers: vec![PeerSet {
            public_key: [2; 32],
            preshared_key: None,
            endpoint: Some("192.0.2.2:51821".parse().unwrap()),
            keepalive: Some(25),
            allowed_ips: vec![("10.20.0.0".parse().unwrap(), 24)],
        }],
        remove: vec![[3; 32]],
    };
    let expected = concat(&[
        &hex("0c010000 1c00 0500 01000000 00000000 0101 0000"),
        &hex(&format!("0800 0200 {WG0}")),
        &hex("2400 0300"),
        &[1; 32],
        &hex("0600 0600 6cca 0000"),
        &hex("c400 0880"),
        // The removed peer: its key and WGPEER_F_REMOVE_ME.
        &hex("3000 0080 2400 0100"),
        &[3; 32],
        &hex("0800 0300 01000000"),
        // The set peer, its allowed IPs replaced.
        &hex("9000 0080 2400 0100"),
        &[2; 32],
        &hex("0800 0300 02000000 2400 0200"),
        &[0; 32],
        &hex("0600 0500 1900 0000"),
        &hex("1400 0400 0200 ca6d c0000202 0000000000000000"),
        &hex("2000 0980 1c00 0080
             0600 0100 0200 0000
             0800 0200 0a140000
             0500 0300 18 000000"),
    ]);
    assert_eq!(set_device(0x1c, "wg0", &device), expected);
}

#[test]
fn set_device_without_peers_or_port_carries_the_key_alone() {
    let device = DeviceSet {
        private_key: [1; 32],
        listen_port: None,
        peers: Vec::new(),
        remove: Vec::new(),
    };
    let expected = concat(&[
        &hex("40000000 1c00 0500 01000000 00000000 0101 0000"),
        &hex(&format!("0800 0200 {WG0}")),
        &hex("2400 0300"),
        &[1; 32],
    ]);
    assert_eq!(set_device(0x1c, "wg0", &device), expected);
}

#[test]
fn set_endpoint_updates_one_existing_peer_only() {
    let expected = concat(&[
        &hex("64000000 1c00 0500 01000000 00000000 0101 0000"),
        &hex(&format!("0800 0200 {WG0}")),
        &hex("4800 0880 4400 0080 2400 0100"),
        &[2; 32],
        &hex("0800 0300 04000000"),
        &hex("1400 0400 0200 ca6c c0000202 0000000000000000"),
    ]);
    assert_eq!(
        set_endpoint(0x1c, "wg0", &[2; 32], "192.0.2.2:51820".parse().unwrap()),
        expected
    );
}

#[test]
fn an_ipv6_endpoint_is_a_sockaddr_in6() {
    let endpoint: SocketAddr = "[2001:db8::1]:51820".parse().unwrap();
    assert_eq!(
        sockaddr(endpoint),
        hex("0a00 ca6c 00000000 20010db8000000000000000000000001 00000000")
    );
}

/// An `RTM_NEWLINK` body: index 7, `name`, link kind `kind`, and `alias`.
fn link_body(name: &str, kind: &str, alias: Option<&str>) -> Vec<u8> {
    let mut body = ifinfomsg(7, 0, 0);
    body.extend(attribute(IFLA_IFNAME, &name_payload(name)));
    if let Some(alias) = alias {
        body.extend(attribute(IFLA_IFALIAS, &name_payload(alias)));
    }
    body.extend(nested(
        IFLA_LINKINFO,
        &[attribute(IFLA_INFO_KIND, &name_payload(kind))],
    ));
    body
}

#[test]
fn only_a_wireguard_link_with_micads_alias_is_micads() {
    assert_eq!(
        owned_link(&link_body("wg0", "wireguard", Some("mica"))),
        Some(("wg0".to_string(), 7))
    );
    assert_eq!(owned_link(&link_body("wg0", "wireguard", None)), None);
    assert_eq!(
        owned_link(&link_body("wg0", "wireguard", Some("wg-easy"))),
        None
    );
    assert_eq!(owned_link(&link_body("br0", "bridge", Some("mica"))), None);
}

#[test]
fn a_dumped_address_counts_on_its_own_link_in_global_scope() {
    let mut body = vec![AF_INET6, 64, 0, RT_SCOPE_UNIVERSE];
    body.extend(7u32.to_ne_bytes());
    body.extend(attribute(
        IFA_ADDRESS,
        &octets("fd00:8::1".parse().unwrap()),
    ));
    assert_eq!(
        link_address(&body, 7),
        Some(("fd00:8::1".parse().unwrap(), 64))
    );
    assert_eq!(link_address(&body, 8), None);
    body[3] = RT_SCOPE_LINK;
    assert_eq!(link_address(&body, 7), None);
}

/// An `RTM_NEWROUTE` body as the kernel dumps it.
fn dumped_route(table: u8, protocol: u8, out_link: u32, destination: Option<&str>) -> Vec<u8> {
    let length = if destination.is_some() { 24 } else { 0 };
    let mut body = vec![
        AF_INET,
        length,
        0,
        0,
        table,
        protocol,
        RT_SCOPE_LINK,
        RTN_UNICAST,
    ];
    body.extend(0u32.to_ne_bytes());
    body.extend(attribute(RTA_TABLE, &u32::from(table).to_ne_bytes()));
    if let Some(destination) = destination {
        body.extend(attribute(RTA_DST, &octets(destination.parse().unwrap())));
    }
    body.extend(attribute(RTA_PREFSRC, &octets("10.8.0.1".parse().unwrap())));
    body.extend(attribute(RTA_OIF, &out_link.to_ne_bytes()));
    body
}

#[test]
fn a_dumped_route_out_of_the_link_reads_back_with_its_protocol() {
    let route = TunnelRoute {
        destination: ("10.20.0.0".parse().unwrap(), 24),
        gateway: None,
        source: Some("10.8.0.1".parse().unwrap()),
        metric: 0,
    };
    assert_eq!(
        link_route(
            &dumped_route(RT_TABLE_MAIN, RTPROT_MICA, 7, Some("10.20.0.0")),
            7
        ),
        Some((route, RTPROT_MICA))
    );
    // Another link's, another table's.
    assert_eq!(
        link_route(&dumped_route(RT_TABLE_MAIN, 4, 8, Some("10.20.0.0")), 7),
        None
    );
    assert_eq!(
        link_route(&dumped_route(255, 2, 7, Some("10.8.0.1")), 7),
        None
    );
    // No destination is the default route.
    let default = link_route(&dumped_route(RT_TABLE_MAIN, 4, 7, None), 7).unwrap();
    assert_eq!(default.0.destination, ("0.0.0.0".parse().unwrap(), 0));
    assert_eq!(default.1, 4);
}

#[test]
fn the_kernel_metric_is_ipv6s_1024_when_none_is_asked() {
    let v4: IpAddr = "10.0.0.0".parse().unwrap();
    let v6: IpAddr = "fd00::".parse().unwrap();
    assert_eq!(kernel_metric(v4, None), 0);
    assert_eq!(kernel_metric(v4, Some(50)), 50);
    assert_eq!(kernel_metric(v6, None), 1024);
    assert_eq!(kernel_metric(v6, Some(0)), 1024);
    assert_eq!(kernel_metric(v6, Some(50)), 50);
}
