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
