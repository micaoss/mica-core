use super::*;

fn nested(kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    attribute(kind | 0x8000, &children.concat())
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

fn can_link_body() -> Vec<u8> {
    let mut stats = [0u64; 24];
    stats[0] = 1200; // rx_packets
    stats[1] = 800; // tx_packets
    stats[4] = 3; // rx_errors
    stats[5] = 1; // tx_errors
    stats[6] = 2; // rx_dropped
    stats[11] = 4; // rx_over_errors
    let stats: Vec<u8> = stats.iter().flat_map(|value| value.to_ne_bytes()).collect();
    let mut berr = 7u16.to_ne_bytes().to_vec();
    berr.extend(9u16.to_ne_bytes());
    let data = nested(
        IFLA_INFO_DATA,
        &[
            attribute(
                IFLA_CAN_BITTIMING,
                &words(&[500_000, 875, 125, 6, 7, 2, 1, 4]),
            ),
            attribute(IFLA_CAN_STATE, &words(&[1])),
            attribute(IFLA_CAN_BERR_COUNTER, &berr),
        ],
    );
    let mut body = vec![0u8; IFINFOMSG];
    body.extend(attribute(IFLA_IFNAME, b"can0\0"));
    body.extend(attribute(IFLA_STATS64, &stats));
    body.extend(nested(
        IFLA_LINKINFO,
        &[
            attribute(IFLA_INFO_KIND, b"can\0"),
            data,
            attribute(IFLA_INFO_XSTATS, &words(&[11, 5, 2, 1, 0, 1])),
        ],
    ));
    body
}

#[test]
fn a_can_link_reads_its_state_timing_and_counters() {
    assert_eq!(
        can_json(&can_link_body()).unwrap(),
        json!({
            "available": true,
            "busState": "error-warning",
            "bitrate": 500_000,
            "samplePoint": 0.875,
            "txErrors": 7,
            "rxErrors": 9,
            "warningCount": 5,
            "errorPassiveCount": 2,
            "busOffCount": 1,
            "rx": { "packets": 1200, "dropped": 2, "overruns": 4, "errors": 3 },
            "tx": { "packets": 800, "dropped": 0, "errors": 1 },
        })
    );
}

#[test]
fn a_link_that_is_not_can_reads_as_none() {
    let mut body = vec![0u8; IFINFOMSG];
    body.extend(nested(
        IFLA_LINKINFO,
        &[attribute(IFLA_INFO_KIND, b"vlan\0")],
    ));
    assert_eq!(can_json(&body), None);
    assert_eq!(can_json(&[0u8; 4]), None);
}

#[test]
fn a_truncated_attribute_ends_the_walk() {
    let mut bytes = attribute(1, b"abcd");
    bytes.extend_from_slice(&[40, 0, 2, 0, 1]);
    assert_eq!(attributes(&bytes), vec![(1, &b"abcd"[..])]);
}

fn peer(key: u8, handshake: u64, rx: u64, tx: u64, keepalive: u16) -> Vec<u8> {
    let mut timespec = handshake.to_ne_bytes().to_vec();
    timespec.extend(0u64.to_ne_bytes());
    nested(
        0,
        &[
            attribute(WGPEER_A_PUBLIC_KEY, &[key; 32]),
            attribute(2, &[0xee; 32]), // the pre-shared key, never kept
            attribute(
                WGPEER_A_PERSISTENT_KEEPALIVE_INTERVAL,
                &keepalive.to_ne_bytes(),
            ),
            attribute(WGPEER_A_LAST_HANDSHAKE_TIME, &timespec),
            attribute(WGPEER_A_RX_BYTES, &rx.to_ne_bytes()),
            attribute(WGPEER_A_TX_BYTES, &tx.to_ne_bytes()),
        ],
    )
}

#[test]
fn a_wireguard_reply_reads_each_peer_and_no_key_but_the_public_one() {
    let mut body = vec![WG_CMD_GET_DEVICE, WG_GENL_VERSION, 0, 0];
    body.extend(attribute(WGDEVICE_A_IFNAME, b"wg0\0"));
    body.extend(attribute(3, &[0xdd; 32])); // the device's private key
    body.extend(nested(
        WGDEVICE_A_PEERS,
        &[
            peer(1, 1_792_000_000, 4096, 2048, 25),
            peer(2, 0, 0, 148, 0),
        ],
    ));
    let peers = wireguard_peers(&body);
    assert_eq!(
        peers
            .iter()
            .map(|peer| peer.to_json(|_| None))
            .collect::<Vec<_>>(),
        vec![
            json!({
                "publicKey": base64::engine::general_purpose::STANDARD.encode([1u8; 32]),
                "latestHandshake": 1_792_000_000u64,
                "rxBytes": 4096,
                "txBytes": 2048,
                "persistentKeepalive": 25,
                "endpoint": null,
                "allowedIps": [],
            }),
            json!({
                "publicKey": base64::engine::general_purpose::STANDARD.encode([2u8; 32]),
                "latestHandshake": null,
                "rxBytes": 0,
                "txBytes": 148,
                "persistentKeepalive": 0,
                "endpoint": null,
                "allowedIps": [],
            }),
        ]
    );
}

#[test]
fn the_family_id_is_read_from_the_controller_reply() {
    let mut body = vec![1, 2, 0, 0];
    body.extend(attribute(CTRL_ATTR_FAMILY_NAME, b"wireguard\0"));
    body.extend(attribute(CTRL_ATTR_FAMILY_ID, &29u16.to_ne_bytes()));
    assert_eq!(family_id(&body), Some(29));
}

#[test]
fn a_dump_reply_is_split_into_its_messages() {
    let mut datagram = message(16, NLM_F_MULTI, b"abcd");
    datagram.extend(message(NLMSG_DONE, NLM_F_MULTI, &0u32.to_ne_bytes()));
    let found = messages(&datagram);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].0, 16);
    assert_eq!(found[0].2, b"abcd");
    assert_eq!(found[1].0, NLMSG_DONE);
}

fn address_body(family: u8, prefix: u8, scope: u8, local: [u8; 4], label: &str) -> Vec<u8> {
    let mut body = vec![family, prefix, 0, scope];
    body.extend(7u32.to_ne_bytes());
    body.extend(attribute(1, &local));
    body.extend(attribute(IFA_LOCAL, &local));
    body.extend(attribute(IFA_LABEL, &name_payload(label)));
    body
}

#[test]
fn an_ipv4_address_is_its_interface_and_the_address_with_its_prefix() {
    assert_eq!(
        ipv4_address(&address_body(2, 24, 0, [10, 0, 0, 5], "eth0")),
        Some(("eth0".to_string(), "10.0.0.5/24".to_string()))
    );
    assert_eq!(
        ipv4_address(&address_body(2, 24, 0, [10, 0, 0, 6], "eth0:1")),
        Some(("eth0".to_string(), "10.0.0.6/24".to_string())),
        "an alias label names its interface"
    );
    assert_eq!(
        ipv4_address(&address_body(2, 8, 254, [127, 0, 0, 1], "lo")),
        None
    );
    assert_eq!(
        ipv4_address(&address_body(10, 64, 0, [0, 0, 0, 0], "eth0")),
        None
    );
    assert_eq!(ipv4_address(&[2, 24]), None);
}

#[test]
fn a_route_reply_reads_its_output_link_and_gateway() {
    let mut body = vec![AF_INET, 32, 0, 0, 254, 16, 0, 1, 0, 0, 0, 0];
    body.extend(attribute(RTA_DST, &[1, 1, 1, 1]));
    body.extend(attribute(RTA_OIF, &words(&[3])));
    body.extend(attribute(RTA_GATEWAY, &[192, 0, 2, 1]));
    assert_eq!(
        route_choice(&body),
        Some(RouteChoice {
            interface_index: 3,
            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
        })
    );
    let mut on_link = vec![AF_INET, 32, 0, 0, 254, 2, 253, 1, 0, 0, 0, 0];
    on_link.extend(attribute(RTA_OIF, &words(&[4])));
    assert_eq!(
        route_choice(&on_link),
        Some(RouteChoice {
            interface_index: 4,
            gateway: None,
        })
    );
    assert_eq!(route_choice(&body[..RTMSG]), None);
}

#[test]
fn a_route_request_asks_for_one_ipv4_destination() {
    let request = route_request(Ipv4Addr::new(1, 1, 1, 1));
    let (kind, flags, body) = messages(&request)[0];
    assert_eq!(kind, RTM_GETROUTE);
    assert_eq!(flags & NLM_F_DUMP, 0);
    assert_eq!(&body[..2], &[AF_INET, 32]);
    assert_eq!(
        attributes(&body[RTMSG..]),
        vec![(RTA_DST, &[1u8, 1, 1, 1][..])]
    );
}

fn allowed(family: u16, address: &[u8], length: u8) -> Vec<u8> {
    nested(
        0,
        &[
            attribute(WGALLOWEDIP_A_FAMILY, &family.to_ne_bytes()),
            attribute(WGALLOWEDIP_A_IPADDR, address),
            attribute(WGALLOWEDIP_A_CIDR_MASK, &[length]),
        ],
    )
}

fn sockaddr_in(address: [u8; 4], port: u16) -> Vec<u8> {
    let mut bytes = u16::from(AF_INET).to_ne_bytes().to_vec();
    bytes.extend(port.to_be_bytes());
    bytes.extend(address);
    bytes.extend([0; 8]);
    bytes
}

fn sockaddr_in6(address: [u8; 16], port: u16) -> Vec<u8> {
    let mut bytes = u16::from(AF_INET6).to_ne_bytes().to_vec();
    bytes.extend(port.to_be_bytes());
    bytes.extend([0; 4]);
    bytes.extend(address);
    bytes.extend([0; 4]);
    bytes
}

/// The endpoint and allowed IPs of each peer, both families, and a peer
/// continued in a second reply keeps one entry with all its allowed IPs.
#[test]
fn a_wireguard_reply_reads_each_peers_endpoint_and_allowed_ips() {
    let reply = |peers: &[Vec<u8>]| {
        let mut body = vec![WG_CMD_GET_DEVICE, WG_GENL_VERSION, 0, 0];
        body.extend(nested(WGDEVICE_A_PEERS, peers));
        body
    };
    let first = reply(&[
        nested(
            0,
            &[
                attribute(WGPEER_A_PUBLIC_KEY, &[1; 32]),
                attribute(WGPEER_A_ENDPOINT, &sockaddr_in([192, 0, 2, 7], 51820)),
                nested(
                    WGPEER_A_ALLOWEDIPS,
                    &[allowed(u16::from(AF_INET), &[10, 1, 0, 0], 16)],
                ),
            ],
        ),
        nested(
            0,
            &[
                attribute(WGPEER_A_PUBLIC_KEY, &[2; 32]),
                attribute(
                    WGPEER_A_ENDPOINT,
                    &sockaddr_in6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 9).octets(), 443),
                ),
            ],
        ),
    ]);
    let second = reply(&[nested(
        0,
        &[
            attribute(WGPEER_A_PUBLIC_KEY, &[1; 32]),
            nested(
                WGPEER_A_ALLOWEDIPS,
                &[allowed(
                    u16::from(AF_INET6),
                    &Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0).octets(),
                    64,
                )],
            ),
        ],
    )]);
    let mut peers = Vec::new();
    merge_peers(&mut peers, wireguard_peers(&first));
    merge_peers(&mut peers, wireguard_peers(&second));
    let json: Vec<Value> = peers
        .iter()
        .map(|peer| peer.to_json(|address| Some(address.is_ipv4())))
        .collect();
    assert_eq!(json.len(), 2);
    assert_eq!(json[0]["endpoint"], "192.0.2.7:51820");
    assert_eq!(
        json[0]["allowedIps"],
        json!([
            {"prefix": "10.1.0.0/16", "routed": true},
            {"prefix": "fd00::/64", "routed": false},
        ])
    );
    assert_eq!(json[1]["endpoint"], "[2001:db8::9]:443");
    assert_eq!(json[1]["allowedIps"], json!([]));
}

#[test]
fn a_route_request_asks_for_one_ipv6_destination() {
    let address = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
    let request = route_request_to(IpAddr::V6(address));
    let (_, _, body) = messages(&request)[0];
    assert_eq!(&body[..2], &[AF_INET6, 128]);
    assert_eq!(
        attributes(&body[RTMSG..]),
        vec![(RTA_DST, &address.octets()[..])]
    );
    let mut reply = vec![AF_INET6, 128, 0, 0, 254, 3, 0, 1, 0, 0, 0, 0];
    reply.extend(attribute(RTA_OIF, &words(&[7])));
    assert_eq!(route_link_of(&reply), Some(7));
}

/// WG-T15/T16 on a real kernel, in a VM only (`_out`-local kit, not CI): two
/// network namespaces joined by a veth pair, a tunnel in each, the peer's
/// prefixes routed into the tunnel as micad renders them for networkd.
/// The kernel's route for each prefix leaves by the tunnel, a ping crosses
/// it, and the observation reports the endpoint, the allowed IPs routed, a
/// prefix whose route is gone unrouted, and an endpoint inside a tunnel
/// prefix as a loop.
#[test]
#[ignore = "needs root, ip, wg and ping in a VM"]
fn vm_wireguard() {
    use std::process::Command;

    // Each tool by its own name and not through a shell: busybox's shell
    // would run its own `ip` applet.
    fn run(line: &str) -> String {
        let mut words = line.split_whitespace();
        let output = Command::new(words.next().unwrap())
            .args(words)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{line}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
    let index = |name: &str| -> u32 {
        std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };

    let near = run("wg genkey");
    let far = run("wg genkey");
    let public = |private: &str| {
        use std::io::Write as _;
        let mut child = Command::new("wg")
            .arg("pubkey")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(private.as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    };
    let near_public = public(&near);
    let far_public = public(&far);
    std::fs::write("/run/near.key", &near).unwrap();
    std::fs::write("/run/far.key", &far).unwrap();
    for line in [
        "ip link set lo up",
        "ip netns add far",
        "ip netns exec far ip link set lo up",
        "ip link add v0 type veth peer name v1 netns far",
        "ip addr add 192.0.2.1/24 dev v0",
        "ip link set v0 up",
        "ip netns exec far ip addr add 192.0.2.2/24 dev v1",
        "ip netns exec far ip link set v1 up",
        "ip link add wg0 type wireguard",
        "wg set wg0 private-key /run/near.key listen-port 51820",
        "ip addr add 10.8.0.1/24 dev wg0",
        "ip addr add fd00:8::1/64 dev wg0",
        "ip link set wg0 up",
        "ip netns exec far ip link add wg1 type wireguard",
        "ip netns exec far wg set wg1 private-key /run/far.key listen-port 51821",
        "ip netns exec far ip addr add 10.8.0.2/24 dev wg1",
        "ip netns exec far ip addr add fd00:8::2/64 dev wg1",
        "ip netns exec far ip addr add 10.20.0.1/24 dev lo",
        "ip netns exec far ip link set wg1 up",
    ] {
        run(line);
    }
    run(&format!(
        "ip netns exec far wg set wg1 peer {near_public} allowed-ips 10.8.0.1/32,fd00:8::1/128"
    ));
    run(&format!(
        "wg set wg0 peer {far_public} endpoint 192.0.2.2:51821 allowed-ips 10.8.0.2/32,10.20.0.0/24,fd00:8::2/128,fd00:20::/64"
    ));
    // What the rendered [Route] sections give networkd to add: the peer's
    // prefixes beyond the tunnel's own subnets.
    run("ip route add 10.20.0.0/24 dev wg0 src 10.8.0.1");
    run("ip -6 route add fd00:20::/64 dev wg0");

    let wg0 = index("wg0");
    // T15: the kernel sends each peer prefix out of the tunnel.
    for destination in ["10.8.0.2", "10.20.0.5", "fd00:8::2", "fd00:20::5"] {
        let address: IpAddr = destination.parse().unwrap();
        assert_eq!(route_link(address).unwrap(), Some(wg0), "{destination}");
    }
    println!("VM_STEP routes PASS");
    // T16: a ping through the tunnel to the far side's routed prefix.
    run("ping -c 2 -W 2 10.20.0.1");
    println!("VM_STEP ping PASS");

    let observe = || {
        let peers = wireguard_device("wg0").unwrap();
        assert_eq!(peers.len(), 1);
        peers[0].to_json(|address| Some(route_link(address).ok()? == Some(wg0)))
    };
    let seen = observe();
    println!("VM wireguard: {seen}");
    assert_eq!(seen["publicKey"], far_public);
    assert_eq!(seen["endpoint"], "192.0.2.2:51821");
    assert!(seen["latestHandshake"].as_u64().is_some());
    let routed = seen["allowedIps"].as_array().unwrap();
    assert_eq!(routed.len(), 4);
    assert!(routed.iter().all(|ip| ip["routed"] == true), "{seen}");

    // A rendered route gone: that prefix, and only it, is unrouted.
    run("ip route del 10.20.0.0/24 dev wg0");
    let seen = observe();
    let unrouted: Vec<&str> = seen["allowedIps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|ip| ip["routed"] != true)
        .map(|ip| ip["prefix"].as_str().unwrap())
        .collect();
    assert_eq!(unrouted, ["10.20.0.0/24"]);
    println!("VM_STEP unrouted PASS");

    // An endpoint inside a prefix the tunnel routes is a loop.
    run(&format!(
        "wg set wg0 peer {far_public} endpoint 10.20.0.9:51821"
    ));
    let seen = observe();
    let peer: micad_settings::WireguardPeer = serde_json::from_value(json!({
        "publicKey": far_public,
        "endpoint": "far.example.net:51821",
        "allowedIps": ["10.8.0.2/32", "10.20.0.0/24", "fd00:8::2/128", "fd00:20::/64"],
    }))
    .unwrap();
    assert_eq!(
        micad_settings::wireguard_peer_state(
            std::slice::from_ref(&peer),
            &peer,
            seen["endpoint"].as_str()
        ),
        "endpoint_route_loop"
    );
    println!("VM_STEP loop PASS");
    println!("VM_WIREGUARD PASS");
}
