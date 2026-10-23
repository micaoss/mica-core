//! Wireless association, levels, addresses and radios.

use serde_json::json;

use super::*;

#[test]
pub(super) fn wpa_status_and_signal_poll_parse_into_an_association() {
    let status = "bssid=aa:bb:cc:dd:ee:ff\nfreq=5180\nssid=Lab\nid=0\nmode=station\n\
        pairwise_cipher=CCMP\ngroup_cipher=CCMP\nkey_mgmt=WPA2-PSK\nwpa_state=COMPLETED\n\
        ip_address=10.0.0.5\naddress=11:22:33:44:55:66\nuuid=abc\n";
    let mut association = parse_wpa_status("wlan0", status);
    assert_eq!(association.state.as_deref(), Some("COMPLETED"));
    assert_eq!(association.ssid.as_deref(), Some("Lab"));
    assert_eq!(association.bssid.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
    assert_eq!(association.frequency_mhz, Some(5180));
    assert_eq!(association.key_management.as_deref(), Some("WPA2-PSK"));
    assert_eq!(association.detail, None);
    apply_signal_poll(
        &mut association,
        "RSSI=-51\nLINKSPEED=433\nNOISE=9999\nFREQUENCY=5180\n",
    );
    assert_eq!(association.rssi_dbm, Some(-51));
    assert_eq!(association.link_speed_mbps, Some(433));

    let rendered = wifi_json(&association);
    assert_eq!(rendered["associated"], true);
    assert_eq!(rendered["ssid"], "Lab");
    assert_eq!(rendered["rssiDbm"], -51);
    // The interface's own MAC and IP are not association facts.
    assert!(rendered.get("address").is_none());
    assert!(rendered.to_string().contains("11:22:33").not());

    let disconnected = parse_wpa_status(
        "wlan0",
        "wpa_state=DISCONNECTED\naddress=11:22:33:44:55:66\n",
    );
    assert_eq!(wifi_json(&disconnected)["associated"], false);
    assert_eq!(wifi_json(&disconnected)["available"], true);

    let garbage = parse_wpa_status("wlan0", "FAIL\n");
    assert_eq!(wifi_json(&garbage)["available"], false);
}

pub(super) trait Not {
    fn not(self) -> bool;
}

impl Not for bool {
    fn not(self) -> bool {
        !self
    }
}

#[test]
pub(super) fn proc_net_wireless_yields_the_level_per_interface() {
    let text = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
        face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n \
        wlan0: 0000   58.  -52.  -256        0      0      0      0      0        0\n";
    let levels = parse_proc_net_wireless(text);
    assert_eq!(levels.get("wlan0"), Some(&-52));
    assert!(parse_proc_net_wireless("").is_empty());
}

#[test]
pub(super) fn addresses_format_by_family_or_by_length() {
    assert_eq!(
        format_address(Some(2), &[192, 0, 2, 7]).as_deref(),
        Some("192.0.2.7")
    );
    assert_eq!(
        format_address(None, &[192, 0, 2, 7]).as_deref(),
        Some("192.0.2.7")
    );
    let mut v6 = [0u8; 16];
    v6[15] = 1;
    assert_eq!(format_address(Some(10), &v6).as_deref(), Some("::1"));
    assert_eq!(format_address(Some(2), &[1, 2]), None);
    assert_eq!(format_address(Some(99), &[0; 4]), None);
}

/// Radios from a fixture sysfs: a phy80211 interface, a Bluetooth
/// adapter, a wwan interface typed by uevent — and the cellular answer
/// is `supported: false` whether or not a modem is plugged in.
#[test]
pub(super) fn radios_are_discovered_and_cellular_is_explicitly_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("sys/class/net/eth0")).unwrap();
    std::fs::create_dir_all(root.join("sys/class/net/wlan0/phy80211")).unwrap();
    std::fs::create_dir_all(root.join("sys/class/net/wwx0")).unwrap();
    std::fs::write(
        root.join("sys/class/net/wwx0/uevent"),
        "INTERFACE=wwx0\nDEVTYPE=wwan\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("sys/class/bluetooth/hci0")).unwrap();
    let radios = radio_evidence(root);
    assert_eq!(radios.wifi_interfaces, vec!["wlan0"]);
    assert_eq!(radios.bluetooth_adapters, vec!["hci0"]);
    assert_eq!(radios.wwan_interfaces, vec!["wwx0"]);

    let observed = observed_json(
        Err("x"),
        &WifiEvidence::default(),
        None,
        &radios,
        &LinkEvidence::default(),
    );
    assert_eq!(observed["capabilities"]["cellular"]["supported"], false);
    assert_eq!(
        observed["capabilities"]["cellular"]["interfaces"],
        json!(["wwx0"])
    );
    assert_eq!(observed["capabilities"]["bluetooth"]["supported"], true);
    assert_eq!(observed["capabilities"]["wifi"]["supported"], true);

    // And the empty board says every radio is unsupported.
    let none = observed_json(
        Err("x"),
        &WifiEvidence::default(),
        None,
        &RadioEvidence::default(),
        &LinkEvidence::default(),
    );
    assert_eq!(none["capabilities"]["wifi"]["supported"], false);
    assert_eq!(none["capabilities"]["bluetooth"]["supported"], false);
    assert_eq!(none["capabilities"]["cellular"]["supported"], false);
    assert_eq!(none["wifi"]["available"], false);
}

/// A scanned network's channel from its frequency, and its security from
/// its flags, in the access point's terms.
#[test]
pub(super) fn a_scan_row_reads_its_channel_and_security() {
    for (frequency, channel) in [
        (2412, Some(1)),
        (2472, Some(13)),
        (2484, Some(14)),
        (5180, Some(36)),
        (5955, Some(1)),
        (900, None),
    ] {
        assert_eq!(channel_of(frequency), channel, "{frequency}");
    }
    for (flags, security) in [
        ("[WPA2-PSK-CCMP][ESS]", "wpa2-psk"),
        ("[WPA2-PSK+SAE-CCMP][ESS]", "wpa2-wpa3"),
        ("[RSN-SAE-CCMP][ESS]", "wpa3-sae"),
        ("[WPA-PSK-TKIP][ESS]", "wpa-psk"),
        ("[WPA2-EAP-CCMP][ESS]", "wpa2-eap"),
        ("[WEP][ESS]", "wep"),
        ("[ESS]", "open"),
    ] {
        assert_eq!(scan_security(flags), security, "{flags}");
    }
}

/// hostapd's `STATUS` names the channel the access point runs on, its own
/// pick when the configuration leaves it automatic; while it is still
/// choosing, the channel is 0 and is not reported.
#[test]
pub(super) fn an_access_points_status_reads_its_channel() {
    let status = "state=ENABLED\nphy=phy0\nfreq=2437\nnum_sta_non_erp=0\nchannel=6\n\
        secondary_channel=0\nssid[0]=mica-lab\n";
    assert_eq!(parse_ap_status(status), (Some(6), Some(2437)));
    assert_eq!(
        parse_ap_status("state=ACS\nfreq=0\nchannel=0\n"),
        (None, None)
    );

    let evidence = ApEvidence {
        interface: "wlan0".to_string(),
        available: true,
        channel: Some(6),
        frequency_mhz: Some(2437),
        ..ApEvidence::default()
    };
    let rendered = super::wifi::access_point_json(&evidence, None);
    assert_eq!(rendered["channel"], 6);
    assert_eq!(rendered["frequencyMhz"], 2437);
    let choosing = ApEvidence {
        interface: "wlan0".to_string(),
        available: true,
        ..ApEvidence::default()
    };
    let rendered = super::wifi::access_point_json(&choosing, None);
    assert!(rendered.get("channel").is_none());
    assert!(rendered.get("frequencyMhz").is_none());
}
