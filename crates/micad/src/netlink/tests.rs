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
            .map(WireguardPeerObservation::to_json)
            .collect::<Vec<_>>(),
        vec![
            json!({
                "publicKey": base64::engine::general_purpose::STANDARD.encode([1u8; 32]),
                "latestHandshake": 1_792_000_000u64,
                "rxBytes": 4096,
                "txBytes": 2048,
                "persistentKeepalive": 25,
            }),
            json!({
                "publicKey": base64::engine::general_purpose::STANDARD.encode([2u8; 32]),
                "latestHandshake": null,
                "rxBytes": 0,
                "txBytes": 148,
                "persistentKeepalive": 0,
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
