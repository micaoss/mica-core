use super::*;

#[tokio::test]
pub(super) async fn on_openrc_the_station_service_is_told_its_interface_and_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let env = dir.path().join("wifi-client.env");
    let reconciler = WifiClientReconciler::openrc_at(
        dir.path().join("wpa_supplicant"),
        env.clone(),
        MockUnitControl::new("inactive", "static"),
    );
    let state = reconciler
        .apply(&settings_with(one_psk_network()))
        .await
        .unwrap();
    assert_eq!(state["unit"], "mica-wifi-client.service");
    assert_eq!(state["networkdUnit"], serde_json::Value::Null);
    let config = dir.path().join("wpa_supplicant/wpa_supplicant-wlan0.conf");
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        format!(
            "# Managed by micad from wifi.client. Do not edit.\ninterface=wlan0\nconfig={}\n",
            config.display()
        )
    );
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        reconciler.control.calls(),
        ["start mica-wifi-client.service"]
    );
}
