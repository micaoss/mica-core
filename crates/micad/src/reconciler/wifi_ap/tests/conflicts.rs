//! The access point against the station on one radio.

use super::*;

#[tokio::test]
pub(super) async fn an_access_point_on_the_station_s_radio_reports_conflict_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with_station(lab_ap(), "wlan0"))
        .await
        .unwrap();

    assert_eq!(state["accessPoint"], json!("conflict"));
    assert!(
        reconciler.control.calls().is_empty(),
        "the reconciler fought the station for the radio: {:?}",
        reconciler.control.calls()
    );
    assert!(
        reconciler.reloader.calls().is_empty(),
        "a conflicting configuration still touched the network stack"
    );
    assert!(
        !paths.config.exists() && !paths.config_dir.exists(),
        "a conflicting configuration still wrote a hostapd config"
    );
    assert!(
        !paths.network_dir.exists(),
        "a conflicting configuration still wrote a networkd unit"
    );
    let rendered = serde_json::to_string(&state).unwrap();
    assert!(
        rendered.contains("wifi.client is enabled on wlan0"),
        "the conflict must say what is wrong: {rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_conflict_does_not_stop_the_station_s_unit() {
    let dir = tempfile::tempdir().unwrap();
    // The station is up and running on this radio, which is exactly the
    // state the AP reconciler must not "fix".
    let (reconciler, _paths) = fixture(dir.path(), "active", "enabled");

    reconciler
        .apply(&settings_with_station(lab_ap(), "wlan0"))
        .await
        .unwrap();

    assert!(
        reconciler.control.calls().is_empty(),
        "stopping a unit the station reconciler owns would flap forever: {:?}",
        reconciler.control.calls()
    );
}

#[tokio::test]
pub(super) async fn a_station_on_a_different_radio_is_not_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with_station(lab_ap(), "wlan1"))
        .await
        .unwrap();

    assert_eq!(
        state["accessPoint"],
        json!("applied"),
        "two radios can carry both roles; refusing that is as wrong as flapping"
    );
    assert!(paths.config.exists());
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service".to_string(),
            "start hostapd@wlan0.service".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn a_disabled_station_on_the_same_radio_is_not_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");
    let mut settings = settings_with_station(lab_ap(), "wlan0");
    settings.wifi.client.enabled = false;

    let state = reconciler.apply(&settings).await.unwrap();

    assert_eq!(
        state["accessPoint"],
        json!("applied"),
        "a station that is switched off is not using the radio"
    );
}

#[tokio::test]
pub(super) async fn an_access_point_that_is_off_never_conflicts_with_the_station() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with_station(
            WifiApSettings {
                mode: ApMode::Off,
                ..lab_ap()
            },
            "wlan0",
        ))
        .await
        .unwrap();

    assert_eq!(
        state["accessPoint"],
        json!("stopped"),
        "an access point that is off is not contending for anything"
    );
}

// ---- secret hygiene ---------------------------------------------------
