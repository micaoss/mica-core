//! The network rules and stored defaults every writer meets in micad's write
//! path, here a settings path over the bus, as `SetSettings` writes it.

use super::wireguard::service_with_wireguard;

/// A default route at a metric the device keeps for itself is refused
/// before it is stored, with its code.
#[tokio::test]
async fn a_reserved_default_route_metric_is_refused_on_the_bus() {
    let (service, _dir) = service_with_wireguard();
    let before = service.get_settings("network").await.expect("settings");
    let err = service
        .persist_setting(
            "network.eth0",
            serde_json::json!({
                "dhcp": false,
                "static": { "address": "10.0.1.10/24" },
                "routes": [{ "destination": "0.0.0.0/0", "gateway": "10.0.1.1", "metric": 5 }],
            }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("route_metric_reserved"), "{err}");
    assert_eq!(service.get_settings("network").await.unwrap(), before);
}

/// A check switched on with no targets is stored with the default targets,
/// whichever writer sent it.
#[tokio::test]
async fn a_check_without_targets_is_stored_with_the_defaults() {
    let (service, _dir) = service_with_wireguard();
    service
        .persist_setting(
            "network.eth0",
            serde_json::json!({ "dhcp": true, "probe": { "enabled": true } }),
        )
        .await
        .expect("the write lands");
    let stored = service
        .get_settings("network.eth0.probe.targets")
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
        serde_json::to_value(micad_settings::default_probe_targets()).unwrap()
    );
}

/// A tunnel's new listen port that a service on the device holds is refused
/// with `port_in_use`, read from the UDP socket table at write time.
#[tokio::test]
async fn a_tunnel_port_a_service_holds_is_refused_on_the_bus() {
    let (service, dir) = service_with_wireguard();
    let proc_net = dir.path().join("net");
    std::fs::create_dir_all(&proc_net).unwrap();
    std::fs::write(
        proc_net.join("udp"),
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
         \x20  0: 00000000:CA76 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 4242 2 0000000000000000 0\n",
    )
    .unwrap();
    let service = service.with_proc_net(proc_net);
    let tunnel = |port: u16| {
        serde_json::json!({
            "kind": "wireguard",
            "dhcp": false,
            "static": { "address": "10.202.0.1/24" },
            "wireguard": { "listenPort": port },
        })
    };
    let err = service
        .persist_setting("network.wg0", tunnel(51830))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("port_in_use"), "{err}");
    service
        .persist_setting("network.wg0", tunnel(51831))
        .await
        .expect("a free port lands");
}
