use super::*;
use serde_json::json;

mod describe;
mod sockets;
mod wireless;

/// A networkd `Describe` the way systemd 257 prints it: a loopback, a
/// DHCP-configured Ethernet port with a default route, and a wireless
/// interface with a static address and no default route.
fn describe_fixture() -> Value {
    json!({
        "Interfaces": [
            {
                "Index": 1, "Name": "lo", "Type": "loopback",
                "AdministrativeState": "unmanaged", "OperationalState": "carrier",
                "CarrierState": "carrier", "OnlineState": null,
                "Addresses": [{"Family": 2, "Address": [127,0,0,1], "PrefixLength": 8, "Scope": 254, "ScopeString": "host", "ConfigSource": "foreign"}],
                "Routes": [{"Family": 2, "Destination": [127,0,0,0], "DestinationPrefixLength": 8, "Scope": 254, "ScopeString": "host", "Protocol": 2, "ProtocolString": "kernel", "Table": 255, "TableString": "local"}]
            },
            {
                "Index": 2, "Name": "eth0", "Type": "ether", "Driver": "stmmac", "MTU": 1500,
                "HardwareAddress": [0x02, 0x42, 0xac, 0x11, 0x00, 0x02],
                "AdministrativeState": "configured", "OperationalState": "routable",
                "CarrierState": "carrier", "OnlineState": "online", "AddressState": "routable",
                "Addresses": [
                    {"Family": 2, "Address": [192,0,2,10], "PrefixLength": 24, "Scope": 0, "ScopeString": "global", "ConfigSource": "DHCPv4", "ConfigProvider": [192,0,2,1]},
                    {"Family": 10, "Address": [0xfe,0x80,0,0,0,0,0,0,0,0x42,0xac,0xff,0xfe,0x11,0,2], "PrefixLength": 64, "Scope": 253, "ScopeString": "link", "ConfigSource": "foreign"}
                ],
                "DNS": [{"Family": 2, "Address": [192,0,2,1], "ConfigSource": "DHCPv4"}],
                "Routes": [
                    {"Family": 2, "Destination": [0,0,0,0], "DestinationPrefixLength": 0, "Gateway": [192,0,2,1], "Priority": 1024, "Scope": 0, "ScopeString": "global", "Protocol": 16, "ProtocolString": "16", "Table": 254, "TableString": "main", "ConfigSource": "DHCPv4"},
                    {"Family": 2, "Destination": [192,0,2,0], "DestinationPrefixLength": 24, "Scope": 253, "ScopeString": "link", "Protocol": 2, "ProtocolString": "kernel", "Table": 254, "TableString": "main"}
                ],
                "DHCPv4Client": {
                    "State": "bound",
                    "Lease": {"Address": [192,0,2,10], "PrefixLength": 24, "ServerAddress": [192,0,2,1], "Router": [[192,0,2,1]], "LifetimeUSec": 86_400_000_000u64}
                }
            },
            {
                "Index": 3, "Name": "wlan0", "Type": "wlan", "Driver": "aic8800",
                "AdministrativeState": "configured", "OperationalState": "no-carrier",
                "CarrierState": "no-carrier", "OnlineState": "offline",
                "Addresses": [{"Family": 2, "Address": [10,0,0,5], "PrefixLength": 24, "Scope": 0, "ScopeString": "global", "ConfigSource": "static"}],
                "Routes": [],
                "UnstableFutureField": "ignored"
            }
        ]
    })
}

/// wpa_supplicant's `SCAN_RESULTS`, as one is actually shaped: a header
/// line, tab-separated rows, and a hidden network whose SSID is empty.
#[test]
fn a_scan_reads_every_row_including_the_one_with_no_name() {
    let text = "bssid / frequency / signal level / flags / ssid\n\
                aa:bb:cc:dd:ee:01\t2437\t-42\t[WPA2-PSK-CCMP][ESS]\tworkshop\n\
                aa:bb:cc:dd:ee:02\t5180\t-71\t[WPA2-PSK-CCMP][ESS]\t\n\
                aa:bb:cc:dd:ee:03\t2462\t-55\t[ESS]\tguest\n";

    let networks = parse_scan_results(text);

    assert_eq!(networks.len(), 3);
    assert_eq!(networks[0].ssid, "workshop");
    assert_eq!(networks[0].frequency_mhz, Some(2437));
    assert_eq!(networks[0].signal_dbm, Some(-42));
    assert!(networks[0].flags.contains("WPA2-PSK"));
    // Hidden: reported with an empty name rather than dropped. The console
    // decides what to do with it.
    assert_eq!(networks[1].ssid, "");
    // Open: no key management in the flags at all.
    assert_eq!(networks[2].flags, "[ESS]");
}

/// A scan on an interface wpa_supplicant is not running on is refused with
/// the reason, not answered with an empty list.
#[test]
fn a_scan_without_a_station_is_refused_rather_than_empty() {
    let root = tempfile::tempdir().expect("tempdir");

    let error = scan_networks(root.path(), "wlan0").expect_err("no station is running");

    assert!(error.contains("wpa_supplicant"), "{error}");
}

/// hostapd's `STA` reply, as one is actually shaped.
#[test]
fn a_station_reply_reads_who_is_connected_and_never_a_credential() {
    let reply = "02:00:00:00:01:00\nflags=[AUTH][ASSOC][AUTHORIZED]\n\
                 connected_time=934\nsignal=-51\nrx_bytes=104857\ntx_bytes=20480\n\
                 dot11RSNAStatsSelectedPairwiseCipher=00-0f-ac-4\n";

    let station = parse_ap_station("02:00:00:00:01:00", reply);

    assert_eq!(station.connected_seconds, Some(934));
    assert_eq!(station.signal_dbm, Some(-51));
    assert_eq!(station.rx_bytes, Some(104_857));
    assert_eq!(station.tx_bytes, Some(20_480));
}

/// The walk ends on what hostapd answers when it is over, and on anything
/// this build does not recognise.
#[test]
fn the_station_walk_ends_rather_than_looping() {
    assert_eq!(
        station_address("02:00:00:00:01:00\nflags=[AUTH]\n"),
        Some("02:00:00:00:01:00".to_string())
    );
    assert_eq!(station_address("FAIL\n"), None);
    assert_eq!(station_address(""), None);
    assert_eq!(station_address("UNKNOWN COMMAND\n"), None);
    assert_eq!(station_address("something else entirely\n"), None);
}

/// An interface with no hostapd on it is reported as one, not as an
/// access point with no clients.
#[test]
fn an_interface_without_hostapd_is_not_an_empty_access_point() {
    let root = tempfile::tempdir().expect("tempdir");

    let evidence = observe_access_point(root.path(), "wlan0");

    assert!(!evidence.available);
    assert!(
        evidence
            .detail
            .is_some_and(|detail| detail.contains("control socket"))
    );
    assert!(evidence.stations.is_empty());
}

/// And a device that runs none reports the member as absent rather than
/// as an empty list.
#[test]
fn a_device_running_no_access_point_says_so() {
    let radios = RadioEvidence {
        wifi_interfaces: vec!["wlan0".to_string()],
        ..RadioEvidence::default()
    };

    let observed = observed_json(Err("x"), &WifiEvidence::default(), None, &radios);

    assert_eq!(observed["accessPoint"]["available"], json!(false));
    assert!(observed["accessPoint"]["detail"].is_string());
}

/// The daemon answers by sending to the path the asker is bound to. micad's
/// unit has a private temporary directory, so a reply socket there is one the
/// daemon cannot reach: it has to sit under the runtime directory both see.
/// The fake below answers only such an asker, with more than one page of
/// results, which a 4096-byte read would cut.
#[test]
fn a_scan_is_answered_on_a_socket_the_daemon_can_reach_and_read_whole() {
    use std::os::unix::net::UnixDatagram;
    let root = tempfile::tempdir().unwrap();
    let control = root.path().join(WPA_CONTROL_DIR);
    std::fs::create_dir_all(&control).unwrap();
    let daemon = UnixDatagram::bind(control.join("wlan0")).unwrap();
    let reachable = root.path().join("run/mica");
    let mut results = String::from("bssid / frequency / signal level / flags / ssid\n");
    for index in 0..60 {
        results.push_str(&format!(
            "aa:bb:cc:dd:ee:{index:02x}\t2412\t-50\t[WPA2-PSK-CCMP][ESS]\tnetwork-with-a-long-name-{index:02}-padding-padding\n"
        ));
    }
    assert!(results.len() > 4096);
    let reply = results.clone();
    let server = std::thread::spawn(move || {
        let mut buffer = [0u8; 256];
        for _ in 0..2 {
            let (received, asker) = daemon.recv_from(&mut buffer).unwrap();
            let path = asker.as_pathname().expect("a bound asker").to_path_buf();
            assert!(path.starts_with(&reachable), "{}", path.display());
            let answer = if &buffer[..received] == b"SCAN" {
                "OK\n"
            } else {
                reply.as_str()
            };
            daemon.send_to(answer.as_bytes(), &path).unwrap();
        }
    });

    let found = scan_networks(root.path(), "wlan0").expect("the daemon answered");

    server.join().unwrap();
    assert_eq!(found.len(), 60);
    assert_eq!(
        found[59].ssid,
        "network-with-a-long-name-59-padding-padding"
    );
}
