use super::*;
use crate::openrc::FakeCommands;

/// `iw phy phy0 info` from a mac80211 radio, as iw 6.x prints it: tab
/// indentation, wrapped combination lines.
const PHY_INFO: &str = "Wiphy phy0
\twiphy index: 0
\tmax # scan SSIDs: 4
\tSupported Ciphers:
\t\t* WEP40 (00-0f-ac:1)
\t\t* TKIP (00-0f-ac:2)
\t\t* CCMP-128 (00-0f-ac:4)
\t\t* CMAC (00-0f-ac:6)
\tSupported interface modes:
\t\t * IBSS
\t\t * managed
\t\t * AP
\t\t * monitor
\tBand 1:
\t\tCapabilities: 0x1862
\t\tFrequencies:
\t\t\t* 2412.0 MHz [1] (20.0 dBm)
\t\t\t* 2467.0 MHz [12] (disabled)
\t\t\t* 2472.0 MHz [13] (disabled)
\t\t\t* 2484.0 MHz [14] (disabled)
\tBand 2:
\t\tFrequencies:
\t\t\t* 5180.0 MHz [36] (23.0 dBm) (no IR)
\tSupported commands:
\t\t * new_interface
\t\t * start_ap
\tvalid interface combinations:
\t\t * #{ managed } <= 1, #{ AP, P2P-client, P2P-GO } <= 1,
\t\t   total <= 2, #channels <= 1
\tHT Capability overrides:
\t\t * MCS: ff ff ff ff ff ff ff ff ff ff
\tDevice supports SAE with AUTHENTICATE command
\tSupported extended features:
\t\t* [ RRM ]: RRM
";

#[test]
pub(super) fn the_phy_says_what_its_access_point_can_offer() {
    let info = parse_phy_info(PHY_INFO);
    assert!(info.access_point);
    assert!(info.sae && info.cmac);
    assert_eq!(info.bands, ["2.4", "5"]);
    assert_eq!(info.channels, [1, 12, 13, 14]);
    assert!(info.ap_client_concurrent);
    assert_eq!(ap_security(&info), ["wpa2-psk", "wpa2-wpa3", "open"]);
}

/// No SAE, or no BIP cipher for protected management frames: no WPA3 mix.
#[test]
pub(super) fn wpa3_needs_sae_and_the_management_frame_cipher() {
    let no_sae = PHY_INFO.replace("\tDevice supports SAE with AUTHENTICATE command\n", "");
    assert_eq!(ap_security(&parse_phy_info(&no_sae)), ["wpa2-psk", "open"]);
    let no_cmac = PHY_INFO.replace("\t\t* CMAC (00-0f-ac:6)\n", "");
    assert_eq!(ap_security(&parse_phy_info(&no_cmac)), ["wpa2-psk", "open"]);
    let offloaded = no_sae.replace(
        "* [ RRM ]: RRM",
        "* [ SAE_OFFLOAD_AP ]: SAE offload support in AP mode",
    );
    assert!(parse_phy_info(&offloaded).sae);
}

#[test]
pub(super) fn a_station_and_an_access_point_at_once_needs_room_for_both() {
    for (entry, concurrent) in [
        (
            "#{ managed } <= 1, #{ AP, P2P-GO } <= 1, total <= 2, #channels <= 1",
            true,
        ),
        (
            "#{ managed } <= 1, #{ AP } <= 1, total <= 1, #channels <= 1",
            false,
        ),
        ("#{ managed, AP } <= 1, total <= 2, #channels <= 1", false),
        ("#{ managed, AP } <= 2, total <= 2, #channels <= 1", true),
        ("#{ managed } <= 2, #{ P2P-client } <= 1, total <= 3", false),
    ] {
        let text = format!("\tvalid interface combinations:\n\t\t * {entry}\n");
        assert_eq!(
            parse_phy_info(&text).ap_client_concurrent,
            concurrent,
            "{entry}"
        );
    }
}

#[test]
pub(super) fn hostapd_built_without_acs_says_so_in_its_binary() {
    let dir = tempfile::tempdir().unwrap();
    let without = dir.path().join("without");
    std::fs::write(
        &without,
        b"\0\0Line %d: tries to enable ACS but CONFIG_ACS disabled\0",
    )
    .unwrap();
    let with = dir.path().join("with");
    std::fs::write(&with, b"\0\0ACS: Automatic channel selection started\0").unwrap();
    assert!(!hostapd_has_acs(&without));
    assert!(hostapd_has_acs(&with));
    assert!(!hostapd_has_acs(&dir.path().join("absent")));
}

/// The radio is asked through its phy; automatic selection needs both the
/// driver's survey and a hostapd built with ACS.
#[tokio::test]
pub(super) async fn the_capabilities_are_asked_of_the_interface_s_phy() {
    let root = tempfile::tempdir().unwrap();
    let phy = root.path().join("sys/class/net/wlan0/phy80211");
    std::fs::create_dir_all(&phy).unwrap();
    std::fs::write(phy.join("name"), "phy0\n").unwrap();
    let fake = FakeCommands::default();
    fake.answer("iw phy phy0 info", 0, PHY_INFO);
    fake.answer(
        "iw dev wlan0 survey dump",
        0,
        "Survey data from wlan0\n\tfrequency:\t\t\t2412 MHz [in use]\n",
    );
    let found = wifi_capabilities(root.path(), &fake, "wlan0")
        .await
        .unwrap();
    assert_eq!(
        found,
        json!({
            "security": ["wpa2-psk", "wpa2-wpa3", "open"],
            "bands": ["2.4", "5"],
            "acs": false,
            "apClientConcurrent": true,
            "channels": [1, 12, 13, 14],
            "accessPoint": true,
        })
    );
    let sbin = root.path().join("usr/sbin");
    std::fs::create_dir_all(&sbin).unwrap();
    std::fs::write(sbin.join("hostapd"), b"ACS: started").unwrap();
    let found = wifi_capabilities(root.path(), &fake, "wlan0")
        .await
        .unwrap();
    assert_eq!(found["acs"], true);

    fake.answer("iw phy phy0 info", 1, "");
    assert!(
        wifi_capabilities(root.path(), &fake, "wlan0")
            .await
            .is_none()
    );
}
