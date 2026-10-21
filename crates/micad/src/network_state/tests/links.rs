use super::*;

fn observe(describe: &Value, wifi: &WifiEvidence, links: &LinkEvidence) -> Value {
    observed_json(Ok(describe), wifi, None, &RadioEvidence::default(), links)
}

/// The lease reads the server and router networkd sourced from DHCPv4, and
/// the time left on the address against the boot clock it is stamped on.
#[test]
pub(super) fn the_lease_names_its_server_router_and_time_left() {
    let links = LinkEvidence {
        boottime_usec: Some(86_400_000_000),
        ..LinkEvidence::default()
    };
    let observed = observe(&describe_fixture(), &WifiEvidence::default(), &links);
    let eth0 = &observed["interfaces"]["entries"][1];
    assert_eq!(eth0["present"], true);
    assert_eq!(
        eth0["lease"],
        json!({"available": true, "server": "192.0.2.1", "router": "192.0.2.1", "remainingSeconds": 3600})
    );
    let wlan0 = &observed["interfaces"]["entries"][2];
    assert_eq!(wlan0["lease"]["available"], false);
}

#[test]
pub(super) fn link_dns_says_where_each_server_came_from() {
    let mut describe = describe_fixture();
    describe["Interfaces"][1]["DNS"] = json!([
        {"Family": 2, "Address": [192,0,2,1], "ConfigSource": "DHCPv4", "ConfigProvider": [192,0,2,1]},
        {"Family": 2, "Address": [9,9,9,9], "ConfigSource": "static"}
    ]);
    let observed = observe(
        &describe,
        &WifiEvidence::default(),
        &LinkEvidence::default(),
    );
    assert_eq!(
        observed["interfaces"]["entries"][1]["linkDns"],
        json!([
            {"address": "192.0.2.1", "source": "DHCPv4", "provider": "192.0.2.1"},
            {"address": "9.9.9.9", "source": "static"}
        ])
    );
}

/// Every route outside the kernel's `local` table, its addresses written out.
#[test]
pub(super) fn routes_list_the_link_s_table_with_addresses_written_out() {
    let observed = observe(
        &describe_fixture(),
        &WifiEvidence::default(),
        &LinkEvidence::default(),
    );
    assert_eq!(
        observed["interfaces"]["entries"][1]["routes"],
        json!([
            {"dest": "0.0.0.0/0", "via": "192.0.2.1", "metric": 1024, "source": "DHCPv4", "table": "main"},
            {"dest": "192.0.2.0/24", "via": null, "metric": null, "source": null, "table": "main"}
        ])
    );
    assert_eq!(observed["interfaces"]["entries"][0]["routes"], json!([]));
}

/// What the kernel said about a CAN or WireGuard link lands on that link.
#[test]
pub(super) fn can_and_wireguard_state_land_on_their_links() {
    let mut describe = describe_fixture();
    describe["Interfaces"].as_array_mut().unwrap().extend([
        json!({"Index": 4, "Name": "can0", "Type": "can", "Routes": []}),
        json!({"Index": 5, "Name": "wg0", "Type": "none", "Kind": "wireguard", "Routes": []}),
    ]);
    let mut links = LinkEvidence::default();
    links.can.insert(
        "can0".to_string(),
        json!({"available": true, "busState": "error-active"}),
    );
    links.wireguard.insert(
        "wg0".to_string(),
        json!({"available": true, "peers": [{"publicKey": "k", "latestHandshake": 1}]}),
    );
    let observed = observe(&describe, &WifiEvidence::default(), &links);
    let entries = &observed["interfaces"]["entries"];
    assert_eq!(entries[3]["can"]["busState"], "error-active");
    assert!(entries[3].get("wireguard").is_none());
    assert_eq!(entries[4]["wireguard"]["peers"][0]["latestHandshake"], 1);
    assert!(entries[1].get("can").is_none());
}

/// A station's address and hostname come from the DHCP server's lease for
/// its MAC; a station with no lease reads without them.
#[test]
pub(super) fn a_station_reads_the_address_and_hostname_it_was_leased() {
    let mut describe = describe_fixture();
    describe["Interfaces"][2]["DHCPServer"] = json!({
        "PoolOffset": 100, "PoolSize": 50,
        "Leases": [{
            "ClientId": [1, 2, 0x5c, 0, 0, 0, 0, 1],
            "Address": [10,0,0,100],
            "Hostname": "phone",
            "HardwareAddress": [0x02,0x5c,0,0,0,1,0,0,0,0,0,0,0,0,0,0]
        }]
    });
    let wifi = WifiEvidence {
        access_points: vec![ApEvidence {
            interface: "wlan0".to_string(),
            available: true,
            detail: None,
            stations: vec![
                ApStation {
                    mac: "02:5C:00:00:00:01".to_string(),
                    ..ApStation::default()
                },
                ApStation {
                    mac: "02:5c:00:00:00:02".to_string(),
                    ..ApStation::default()
                },
            ],
        }],
        ..WifiEvidence::default()
    };
    let observed = observe(&describe, &wifi, &LinkEvidence::default());
    let stations = &observed["accessPoint"]["entries"][0]["stations"];
    assert_eq!(stations[0]["address"], "10.0.0.100");
    assert_eq!(stations[0]["hostname"], "phone");
    assert!(stations[1].get("address").is_none());
}

/// `wifi.association` is the network the radio joined, in the access
/// point's security terms; with none joined it says so.
#[test]
pub(super) fn the_association_is_the_joined_network_or_absent() {
    let joined = WifiAssociation {
        interface: "wlan0".to_string(),
        state: Some("COMPLETED".to_string()),
        ssid: Some("harbour".to_string()),
        bssid: Some("aa:bb:cc:dd:ee:01".to_string()),
        key_management: Some("SAE".to_string()),
        rssi_dbm: Some(-58),
        link_speed_mbps: Some(72),
        ..WifiAssociation::default()
    };
    let wifi = WifiEvidence {
        control_dir_present: true,
        associations: vec![joined.clone()],
        ..WifiEvidence::default()
    };
    let radios = RadioEvidence {
        wifi_interfaces: vec!["wlan0".to_string()],
        ..RadioEvidence::default()
    };
    let observed = observed_json(
        Ok(&describe_fixture()),
        &wifi,
        None,
        &radios,
        &LinkEvidence::default(),
    );
    assert_eq!(
        observed["wifi"]["association"],
        json!({
            "available": true, "interface": "wlan0", "ssid": "harbour",
            "bssid": "aa:bb:cc:dd:ee:01", "signalDbm": -58, "txBitrate": 72,
            "security": "wpa3-sae"
        })
    );
    for (management, security) in [
        ("WPA2-PSK", "wpa2-psk"),
        ("WPA-PSK", "wpa-psk"),
        ("NONE", "open"),
    ] {
        let association = WifiAssociation {
            key_management: Some(management.to_string()),
            ..joined.clone()
        };
        assert_eq!(association_json(&[association])["security"], security);
    }
    let searching = WifiAssociation {
        state: Some("SCANNING".to_string()),
        ..joined
    };
    assert_eq!(association_json(&[searching])["available"], false);
}

fn ap_observed(running: bool, blocked: bool) -> Value {
    let entries = if running {
        json!([{"interface": "wlan0", "stationCount": 0, "stations": []}])
    } else {
        json!([])
    };
    json!({
        "accessPoint": {"available": running, "entries": entries},
        "capabilities": {"wifi": {"supported": true, "blocked": blocked}},
    })
}

fn wifi(mode: micad_settings::ApMode, client: bool) -> micad_settings::WifiSettings {
    let mut wifi = micad_settings::WifiSettings::default();
    wifi.ap.interface = "wlan0".to_string();
    wifi.ap.mode = mode;
    wifi.client.interface = "wlan0".to_string();
    wifi.client.enabled = client;
    wifi
}

fn reason(observed: &Value) -> (&Value, &Value) {
    (
        &observed["accessPoint"]["running"],
        &observed["accessPoint"]["reason"],
    )
}

/// Each cause the access point can be down for, in the order they are
/// checked, and a running one with none.
#[test]
pub(super) fn the_access_point_says_why_it_is_not_running() {
    use micad_settings::ApMode::{Always, Off};
    let mut observed = ap_observed(true, false);
    annotate_access_point(&mut observed, &wifi(Always, false), None, &[]);
    assert_eq!(reason(&observed), (&json!(true), &Value::Null));

    let cases = [
        (wifi(Off, false), false, None, "disabled"),
        (wifi(Off, true), false, None, "client_mode"),
        (wifi(Always, true), false, None, "client_mode"),
        (wifi(Always, false), true, None, "radio_blocked"),
        (
            wifi(Always, false),
            false,
            Some(json!({"error": "start hostapd@wlan0.service"})),
            "driver_error",
        ),
    ];
    for (settings, blocked, state, expected) in cases {
        let mut observed = ap_observed(false, blocked);
        annotate_access_point(&mut observed, &settings, state.as_ref(), &[]);
        assert_eq!(
            reason(&observed),
            (&json!(false), &json!(expected)),
            "{expected}"
        );
    }

    let mut unset = wifi(Always, false);
    unset.ap.country_code = String::new();
    let mut observed = ap_observed(false, false);
    annotate_access_point(&mut observed, &unset, None, &[]);
    assert_eq!(observed["accessPoint"]["reason"], "country_unset");

    let state = json!({"activeState": "failed"});
    let lines = [
        "wlan0: interface state UNINITIALIZED->COUNTRY_UPDATE".to_string(),
        "Channel 13 (primary) not allowed for AP mode, flags: 0x10095b NO-IR".to_string(),
    ];
    let mut observed = ap_observed(false, false);
    annotate_access_point(&mut observed, &wifi(Always, false), Some(&state), &lines);
    assert_eq!(observed["accessPoint"]["reason"], "channel_invalid");
    assert_eq!(observed["accessPoint"]["reasonDetail"], lines[1]);

    // Meant to run, not running and nothing explains it: starting.
    let mut observed = ap_observed(false, false);
    annotate_access_point(
        &mut observed,
        &wifi(Always, false),
        Some(&json!({"activeState": "activating"})),
        &[],
    );
    assert_eq!(reason(&observed), (&json!(false), &Value::Null));
}

#[test]
pub(super) fn a_blocking_wlan_rfkill_switch_is_seen() {
    let root = tempfile::tempdir().unwrap();
    for (name, kind, soft) in [("rfkill0", "bluetooth", "1"), ("rfkill1", "wlan", "0")] {
        let dir = root.path().join("sys/class/rfkill").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        std::fs::write(dir.join("soft"), format!("{soft}\n")).unwrap();
        std::fs::write(dir.join("hard"), "0\n").unwrap();
    }
    assert!(!radio_evidence(root.path()).wifi_blocked);
    std::fs::write(root.path().join("sys/class/rfkill/rfkill1/soft"), "1\n").unwrap();
    assert!(radio_evidence(root.path()).wifi_blocked);
}
