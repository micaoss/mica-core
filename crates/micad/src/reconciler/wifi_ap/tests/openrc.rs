use super::*;

#[test]
pub(super) fn udhcpd_serves_the_pool_networkd_would() {
    assert_eq!(
        render_udhcpd("wlan0", "192.168.4.1/24", "/run/x.pid").unwrap(),
        "# Managed by micad from wifi.ap. Do not edit.\n\
         interface wlan0\nstart 192.168.4.2\nend 192.168.4.254\n\
         option subnet 255.255.255.0\noption router 192.168.4.1\n\
         pidfile /run/x.pid\nlease_file /run/mica/wifi-ap-udhcpd.leases\n"
    );
    assert!(render_udhcpd("wlan0", "192.168.4.255/24", "/run/x.pid").is_err());
}

#[tokio::test]
pub(super) async fn on_openrc_the_service_is_told_its_parameters_and_restarted_on_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let env = dir.path().join("wifi-ap.env");
    let udhcpd = dir.path().join("udhcpd.conf");
    let reconciler = WifiApReconciler::openrc_at(
        dir.path().join("hostapd"),
        env.clone(),
        udhcpd.clone(),
        dir.path().join("state"),
        MockUnitControl::new("active", "static"),
    );
    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();
    assert_eq!(state["unit"], "mica-wifi-ap.service");
    assert_eq!(state["networkdUnit"], serde_json::Value::Null);
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        format!(
            "# Managed by micad from wifi.ap. Do not edit.\n\
             interface=wlan0\nconfig={}\naddress=192.168.4.1/24\nudhcpd={}\n",
            dir.path().join("hostapd/wlan0.conf").display(),
            udhcpd.display()
        )
    );
    assert!(
        std::fs::read_to_string(&udhcpd)
            .unwrap()
            .contains("start 192.168.4.2")
    );
    assert_eq!(reconciler.control.calls(), ["restart mica-wifi-ap.service"]);
    // Nothing changed, nothing restarted.
    reconciler.apply(&settings_with(lab_ap())).await.unwrap();
    assert_eq!(reconciler.control.calls().len(), 1);
}
