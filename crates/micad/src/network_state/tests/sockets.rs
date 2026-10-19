//! The DNS probe and the control sockets.

use serde_json::json;

use super::*;

/// The DNS member across the three probe outcomes, and the resolver
/// being unreachable.
#[test]
pub(super) fn the_dns_member_reports_reachability_and_the_probe_outcome() {
    let resolved = DnsEvidence {
        resolver_reachable: true,
        servers: vec!["192.0.2.1".to_string()],
        probe: Some((
            DNS_PROBE_NAME.to_string(),
            DnsOutcome::Resolved { addresses: 4 },
        )),
    };
    let member = dns_json(Some(&resolved), Vec::new());
    assert_eq!(member["available"], true);
    assert_eq!(member["resolverServers"], json!(["192.0.2.1"]));
    assert_eq!(member["probe"]["reachable"], true);
    assert_eq!(member["probe"]["result"], "resolved");
    assert_eq!(member["probe"]["name"], DNS_PROBE_NAME);

    let failed = DnsEvidence {
        probe: Some((
            "n".to_string(),
            DnsOutcome::Failed("org.freedesktop.resolve1.NoNameServers: x".to_string()),
        )),
        ..resolved.clone()
    };
    assert_eq!(
        dns_json(Some(&failed), Vec::new())["probe"]["reachable"],
        false
    );
    assert_eq!(
        dns_json(Some(&failed), Vec::new())["probe"]["result"],
        "failed"
    );

    let timed_out = DnsEvidence {
        probe: Some(("n".to_string(), DnsOutcome::TimedOut)),
        ..resolved
    };
    assert_eq!(
        dns_json(Some(&timed_out), Vec::new())["probe"]["result"],
        "timeout"
    );

    let unreachable = dns_json(Some(&DnsEvidence::default()), vec!["1.1.1.1".to_string()]);
    assert_eq!(unreachable["available"], false);
    assert_eq!(unreachable["linkServers"], json!(["1.1.1.1"]));
    assert_eq!(unreachable["probe"]["available"], false);
}

/// The control-socket seam, driven end to end against a fake
/// wpa_supplicant listening on a datagram socket in a fixture root: the
/// observer finds the wireless interface in sysfs, asks STATUS and
/// SIGNAL_POLL, and renders the association.
#[tokio::test]
pub(super) async fn the_control_socket_is_asked_for_each_wireless_interface() {
    use std::os::unix::net::UnixDatagram;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("sys/class/net/wlan0/phy80211")).unwrap();
    std::fs::create_dir_all(root.join(WPA_CONTROL_DIR)).unwrap();
    let server = UnixDatagram::bind(root.join(WPA_CONTROL_DIR).join("wlan0")).unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let fake = std::thread::spawn(move || {
        let mut asked = Vec::new();
        for _ in 0..2 {
            let mut buffer = [0u8; 256];
            let (received, peer) = server.recv_from(&mut buffer).unwrap();
            let command = String::from_utf8_lossy(&buffer[..received]).to_string();
            let reply = match command.as_str() {
                "STATUS" => {
                    "wpa_state=COMPLETED\nssid=Lab\nbssid=aa:bb:cc:dd:ee:ff\nfreq=2437\nkey_mgmt=SAE\n"
                }
                "SIGNAL_POLL" => "RSSI=-60\nLINKSPEED=72\n",
                _ => "FAIL\n",
            };
            server
                .send_to(reply.as_bytes(), peer.as_pathname().unwrap())
                .unwrap();
            asked.push(command);
        }
        asked
    });

    let radios = radio_evidence(root);
    let wifi = observe_wifi(root, &radios.wifi_interfaces).await;
    assert_eq!(fake.join().unwrap(), vec!["STATUS", "SIGNAL_POLL"]);
    assert!(wifi.control_dir_present);
    assert_eq!(wifi.associations.len(), 1);
    let association = &wifi.associations[0];
    assert_eq!(association.ssid.as_deref(), Some("Lab"));
    assert_eq!(association.key_management.as_deref(), Some("SAE"));
    assert_eq!(association.rssi_dbm, Some(-60));
    assert_eq!(association.link_speed_mbps, Some(72));

    let observed = observed_json(Err("no networkd in this test"), &wifi, None, &radios);
    assert_eq!(observed["wifi"]["available"], true);
    assert_eq!(observed["wifi"]["associations"][0]["associated"], true);
    assert_eq!(observed["wifi"]["associations"][0]["frequencyMhz"], 2437);
}

/// A wireless interface whose socket nobody serves is reported as not
/// askable, with the reason, and the observation returns within the
/// bound.
#[tokio::test]
pub(super) async fn an_unserved_control_socket_is_absent_with_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("sys/class/net/wlan0/phy80211")).unwrap();
    std::fs::create_dir_all(root.join(WPA_CONTROL_DIR)).unwrap();
    let started = std::time::Instant::now();
    let wifi = observe_wifi(root, &["wlan0".to_string()]).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(wifi.associations.len(), 1);
    assert!(
        wifi.associations[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("could not be asked"))
    );
    let rendered = wifi_json(&wifi.associations[0]);
    assert_eq!(rendered["available"], false);
}

#[tokio::test]
pub(super) async fn the_unavailable_observer_is_an_error() {
    assert!(UnavailableNetworkState.observe().await.is_err());
    assert!(UnavailableNetworkState.describe().await.is_err());
}
