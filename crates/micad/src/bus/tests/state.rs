//! The state tree, settings failures and the transient password.

use super::super::paths_overlap;

use super::*;

#[tokio::test]
pub(super) async fn power_requests_do_not_touch_the_settings_tree() {
    let (service, _calls, _dir) = service_with_mock();
    let before = service.get_settings("").await.expect("settings");

    service.request_reboot(":1.1").await.expect("reboot");
    service.request_power_off(":1.1").await.expect("power off");

    assert_eq!(service.get_settings("").await.expect("settings"), before);
}

/// The uptime graft: `GetState` serves whole seconds since boot at
/// `uptime` and inside the whole tree, computed at read time, while the
/// stored tree — what the item façade projects — stays untouched by the
/// read.
#[tokio::test]
pub(super) async fn get_state_serves_uptime_without_storing_it() {
    let (service, _calls, _dir) = service_with_mock();

    let direct = service.get_state("uptime").await.expect("uptime");
    let direct: u64 = serde_json::from_str(&direct).expect("a bare JSON number");
    let whole = service.get_state("").await.expect("whole tree");
    let whole: serde_json::Value = serde_json::from_str(&whole).expect("json");
    let grafted = whole["uptime"].as_u64().expect("uptime in the whole tree");
    assert!(
        grafted >= direct,
        "uptime went backwards: {grafted} < {direct}"
    );

    let (_settings, state) = service.trees().await;
    assert!(
        state.get("uptime").is_none(),
        "a read must not write the stored tree: {state}"
    );
}

/// Every `SettingsError` variant, against the error name it must travel
/// under: the two conditions the fdo vocabulary cannot separate get
/// interface-scoped names, everything else keeps its standard fdo name.
#[test]
pub(super) fn each_settings_failure_travels_under_its_own_error_name() {
    use micad_settings::SettingsError;
    use zbus::DBusError as _;

    for (err, name) in [
        (
            SettingsError::NotFound("a.path".into()),
            "com.mica.micad1.Error.NotFound",
        ),
        (
            SettingsError::ReadOnly("a.path".into()),
            "com.mica.micad1.Error.ReadOnly",
        ),
        (
            SettingsError::Validation {
                path: "a.path".into(),
                message: "bad".into(),
            },
            "org.freedesktop.DBus.Error.InvalidArgs",
        ),
        (
            SettingsError::Io(std::io::Error::other("disk")),
            "org.freedesktop.DBus.Error.IOError",
        ),
        (
            SettingsError::Parse("mangled".into()),
            "org.freedesktop.DBus.Error.Failed",
        ),
        (
            SettingsError::SchemaVersion("stuck".into()),
            "org.freedesktop.DBus.Error.Failed",
        ),
        // The variant. An IO name and not `InvalidArgs`:
        // the caller asked for something reasonable and the device cannot
        // reach the store. This table is the one place that says what a
        // variant travels as.
        (
            SettingsError::Unavailable {
                directory: "/mica/config".into(),
                mount: "/mica".into(),
            },
            "org.freedesktop.DBus.Error.IOError",
        ),
    ] {
        let message = err.to_string();
        let fault = super::super::to_bus_error(err);
        assert_eq!(fault.name().as_str(), name);
        assert_eq!(
            fault.description(),
            Some(message.as_str()),
            "the description must stay micad's own words ({name})"
        );
    }
}

#[tokio::test]
pub(super) async fn a_transient_password_does_not_touch_the_settings_tree() {
    let (service, _calls, dir) = service_with_mock();
    let before = service.get_settings("").await.expect("settings");

    service
        .set_transient_root_password("correct horse battery")
        .await
        .expect("set transient root password");

    assert_eq!(
        service.get_settings("").await.expect("settings"),
        before,
        "a password must never enter the settings tree"
    );
    assert!(
        !before.contains("correct horse"),
        "the fixture itself must not carry the password"
    );
    assert!(
        !dir.path().join("settings.toml").exists(),
        "nothing was persisted, so no settings file was written at all"
    );
    assert!(
        crate::transient::transient_password_active(&dir.path().join("shadow")),
        "the call must still have done its actual job"
    );
}

#[tokio::test]
pub(super) async fn a_rejected_transient_password_is_an_error_that_does_not_echo_it() {
    let (service, _calls, dir) = service_with_mock();

    let err = service
        .set_transient_root_password("short12")
        .await
        .expect_err("seven bytes is below the floor");

    assert!(
        !err.to_string().contains("short12"),
        "the password leaked into the D-Bus error: {err}"
    );
    assert!(
        !crate::transient::transient_password_active(&dir.path().join("shadow")),
        "a rejected password must leave no marker behind"
    );
}

#[test]
pub(super) fn root_matches_everything() {
    assert!(paths_overlap("", "network"));
    assert!(paths_overlap("network", ""));
    assert!(paths_overlap("", ""));
    assert!(paths_overlap(".", "hostname"));
    assert!(paths_overlap("hostname", "."));
}

#[test]
pub(super) fn prefix_matches_both_directions() {
    assert!(paths_overlap("network.eth0.dhcp", "network"));
    assert!(paths_overlap("network", "network.eth0.dhcp"));
    assert!(paths_overlap("hostname", "hostname"));
}

#[test]
pub(super) fn disjoint_paths_do_not_match() {
    assert!(!paths_overlap("hostname", "network"));
    assert!(!paths_overlap("network.eth0", "network2"));
    assert!(!paths_overlap("net", "network"));
}
