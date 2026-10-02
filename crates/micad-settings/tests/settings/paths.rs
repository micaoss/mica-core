//! Reading and writing the tree by dot-path.

use micad_settings::{
    ApMode, IfaceSettings, ProvisioningState, Settings, SettingsError, StaticConfig,
    WebAdminSettings, WifiNetwork, json_path_get,
};
use serde_json::json;

use super::*;

#[test]
pub(super) fn get_whole_tree_scalar_and_nested() {
    let settings = populated();
    let whole = settings.get("").unwrap();
    assert_eq!(whole, settings.get(".").unwrap());
    assert_eq!(whole["hostname"], json!("mica"));

    assert_eq!(settings.get("hostname").unwrap(), json!("mica"));
    assert_eq!(
        settings.get("network.eth0.static.address").unwrap(),
        json!("192.168.1.10/24")
    );
    assert_eq!(settings.get("network.wlan0.dhcp").unwrap(), json!(true));
}

#[test]
pub(super) fn get_unknown_path_is_not_found() {
    let settings = Settings::default();
    assert!(matches!(
        settings.get("network.eth9.dhcp"),
        Err(SettingsError::NotFound(_))
    ));
    assert!(matches!(
        settings.get("hostname..x"),
        Err(SettingsError::NotFound(_))
    ));
    // `schema_version` is not a path of the tree: the version is on
    // each document. It is what the READ route resolves,
    // so this is the leg that makes `GET /api/v1/settings/schema_version` the
    // same 404 every other absent root gets rather than the named 409 it was.
    assert!(matches!(
        settings.get("schema_version"),
        Err(SettingsError::NotFound(_))
    ));
}

// --- Dot-path set ----------------------------------------------------------

#[test]
pub(super) fn set_scalar_and_create_intermediate_entries() {
    let mut settings = Settings::default();
    settings.set("hostname", json!("edge-1")).unwrap();
    assert_eq!(settings.hostname, "edge-1");

    settings.set("network.eth0.dhcp", json!(true)).unwrap();
    assert_eq!(
        settings.network["eth0"],
        IfaceSettings {
            dhcp: true,
            ..IfaceSettings::default()
        }
    );

    settings.set("network.eth0.dhcp", json!(false)).unwrap();
    settings
        .set("network.eth0.static.address", json!("10.0.0.2/24"))
        .unwrap();
    settings
        .set("network.eth0.static.gateway", json!("10.0.0.1"))
        .unwrap();
    settings
        .set("network.eth0.static.dns", json!(["10.0.0.1"]))
        .unwrap();
    assert_eq!(
        settings.network["eth0"].static_,
        Some(StaticConfig {
            address: "10.0.0.2/24".to_string(),
            gateway: Some("10.0.0.1".to_string()),
            dns: vec!["10.0.0.1".to_string()],
        })
    );
}

#[test]
pub(super) fn set_and_get_web_admin_roundtrip() {
    let mut settings = Settings::default();
    assert!(matches!(
        settings.get("access.webAdmin"),
        Err(SettingsError::NotFound(_))
    ));

    settings
        .set("access.webAdmin", json!({"password_hash": "x"}))
        .unwrap();
    assert_eq!(
        settings.access.web_admin,
        Some(WebAdminSettings {
            password_hash: "x".to_string()
        })
    );
    assert_eq!(
        settings.get("access.webAdmin.password_hash").unwrap(),
        json!("x")
    );
}

#[test]
pub(super) fn set_whole_tree_replaces_settings() {
    let mut settings = Settings::default();
    let replacement = populated();
    settings
        .set(".", serde_json::to_value(&replacement).unwrap())
        .unwrap();
    assert_eq!(settings, replacement);
}

#[test]
pub(super) fn set_errors_leave_state_unchanged() {
    let mut settings = populated();
    let before = settings.clone();

    assert!(matches!(
        settings.set("bogus.path", json!(1)),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("hostname.sub", json!("x")),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("network.eth0.dhcp", json!("yes")),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("hostname", json!(5)),
        Err(SettingsError::Validation { .. })
    ));

    // `schema_version` is not a key of the tree (the version is on
    // each document), so a write naming it is an unknown field
    // rather than a read-only one -- and it still writes nothing.
    let mut stamped = serde_json::to_value(&before).unwrap();
    stamped["schema_version"] = json!(1);
    assert!(matches!(
        settings.set("", stamped),
        Err(SettingsError::Validation { .. })
    ));

    assert_eq!(settings, before);
}

// --- json_path_get ---------------------------------------------------------

#[test]
pub(super) fn json_path_get_navigates_a_live_state_tree() {
    let tree = json!({
        "hostname": {"current": "mica"},
        "network": {"eth0": {"operstate": "up", "addresses": ["10.0.0.2/24"]}},
    });
    assert_eq!(json_path_get(&tree, ""), Some(&tree));
    assert_eq!(json_path_get(&tree, "."), Some(&tree));
    assert_eq!(
        json_path_get(&tree, "network.eth0.operstate"),
        Some(&json!("up"))
    );
    assert_eq!(json_path_get(&tree, "network.eth1"), None);
    assert_eq!(json_path_get(&tree, "hostname.current.deeper"), None);
    assert_eq!(json_path_get(&tree, "network..eth0"), None);
}

/// Every new leaf is reachable through the dot-path API.
#[test]
pub(super) fn dot_path_reaches_every_new_leaf() {
    let mut settings = Settings::default();

    let writes: Vec<(&str, serde_json::Value)> = vec![
        ("access.ssh.enabled", json!(true)),
        ("access.ssh.port", json!(2222)),
        ("access.ssh.permitRootLogin", json!(false)),
        ("access.ssh.passwordAuthentication", json!(false)),
        (
            "access.ssh.listenAddresses",
            json!(["10.0.0.7", "127.0.0.1"]),
        ),
        ("access.console.shellEnabled", json!(true)),
        ("access.device.passwordHash", json!("$argon2id$v=19$x$y$z")),
        ("access.device.generation", json!(4)),
        ("provisioning.state", json!("complete")),
        ("provisioning.deviceId", json!("a1b2c3d4e5f6")),
        ("provisioning.seededGeneration", json!(7)),
        ("wifi.client.enabled", json!(true)),
        ("wifi.client.interface", json!("wlan1")),
        (
            "wifi.client.networks",
            json!([
                {"ssid": "site-ap", "psk": "hunter2hunter2", "hidden": true, "priority": 10},
                {"ssid": "open-ap", "hidden": false, "priority": 0}
            ]),
        ),
        ("wifi.ap.mode", json!("provisioning")),
        ("wifi.ap.interface", json!("wlan1")),
        ("wifi.ap.ssid", json!("appliance-a1b2")),
        ("wifi.ap.psk", json!("provisioning-pin")),
        ("wifi.ap.channel", json!(11)),
        ("wifi.ap.countryCode", json!("CN")),
        ("wifi.ap.address", json!("10.42.0.1/24")),
        ("wifi.ap.holdDownSeconds", json!(30)),
        ("wifi.ap.graceSeconds", json!(15)),
    ];

    // Each write is read back verbatim. `psk` is left out of the second network
    // on purpose: an absent key is how an open network is spelled, and it stays
    // absent on the way out.
    for (path, value) in &writes {
        settings.set(path, value.clone()).unwrap();
        assert_eq!(
            &settings.get(path).unwrap(),
            value,
            "round trip at `{path}`"
        );
    }

    // The writes landed on the typed tree, not just on the JSON projection.
    assert_eq!(settings.access.ssh.port, 2222);
    assert!(settings.access.console.shell_enabled);
    assert_eq!(settings.access.device.generation, 4);
    assert_eq!(settings.provisioning.state, ProvisioningState::Complete);
    assert_eq!(settings.wifi.ap.mode, ApMode::Provisioning);
    assert_eq!(
        settings.wifi.client.networks,
        vec![
            WifiNetwork {
                ssid: "site-ap".to_string(),
                psk: Some("hunter2hunter2".to_string()),
                hidden: true,
                priority: 10,
            },
            WifiNetwork {
                ssid: "open-ap".to_string(),
                psk: None,
                hidden: false,
                priority: 0,
            },
        ]
    );
}

/// R3.5 rejection case: an out-of-domain enum value is refused and leaves the
/// tree untouched.
#[test]
pub(super) fn set_rejects_unknown_ap_mode_and_leaves_settings_unchanged() {
    let mut settings = Settings::default();
    settings.set("wifi.ap.mode", json!("always")).unwrap();
    let before = settings.clone();

    let err = settings.set("wifi.ap.mode", json!("captive")).unwrap_err();
    assert!(
        matches!(&err, SettingsError::Validation { path, .. } if path == "wifi.ap.mode"),
        "expected a validation error at `wifi.ap.mode`, got {err:?}"
    );
    assert_eq!(settings, before);
    assert_eq!(settings.wifi.ap.mode, ApMode::Always);

    // Same for the other new enum, and for a mistyped scalar.
    assert!(matches!(
        settings.set("provisioning.state", json!("half")),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("access.ssh.port", json!("22")),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("wifi.client.networks", json!([{"psk": "x"}])),
        Err(SettingsError::Validation { .. })
    ));
    assert_eq!(settings, before);
}

// --- Schema v4: access.ssh.authorizedKeys ----------------------------------
