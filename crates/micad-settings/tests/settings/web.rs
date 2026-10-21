//! Where apid listens: `access.web`.

use micad_settings::{SYSTEM_DOCUMENT, Settings, SettingsError, WebMode, WebSettings};
use serde_json::json;

use super::{config_document, store_at};

/// Off the well-known ports: plain HTTP on 8080, HTTPS off and on 8443 when
/// it is turned on.
#[test]
pub(super) fn the_console_listens_on_8080_by_default() {
    assert_eq!(
        Settings::default().access.web,
        WebSettings {
            mode: WebMode::Network,
            http_port: 8080,
            https_enabled: false,
            https_port: 8443,
        }
    );
}

/// `system.json` carries `web` only once it is not the default.
#[test]
pub(super) fn system_json_writes_the_listeners_only_when_they_moved() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let mut settings = Settings::default();
    store.save(&settings).unwrap();
    assert!(config_document(&dir, SYSTEM_DOCUMENT).get("web").is_none());

    settings
        .set(
            "access.web",
            json!({ "httpPort": 8081, "httpsEnabled": true, "httpsPort": 9443 }),
        )
        .unwrap();
    store.save(&settings).unwrap();
    assert_eq!(
        config_document(&dir, SYSTEM_DOCUMENT)["web"],
        json!({ "mode": "network", "httpPort": 8081, "httpsEnabled": true, "httpsPort": 9443 })
    );
    assert_eq!(store.load().unwrap().access.web, settings.access.web);
}

/// A write may not put two listeners on one port, or a listener on port 0.
#[test]
pub(super) fn a_write_that_would_put_two_listeners_on_one_port_is_refused() {
    let refused = |settings: &mut Settings, path: &str, value: serde_json::Value| {
        let err = settings.set(path, value).unwrap_err();
        assert!(matches!(err, SettingsError::Validation { .. }), "{err}");
        err.to_string()
    };
    let mut settings = Settings::default();
    let message = refused(
        &mut settings,
        "access.web",
        json!({ "httpPort": 8443, "httpsEnabled": true, "httpsPort": 8443 }),
    );
    assert!(message.contains("access.web.httpsPort 8443"), "{message}");
    refused(&mut settings, "access.web.httpPort", json!(0));

    settings.set("access.ssh.enabled", json!(true)).unwrap();
    refused(&mut settings, "access.web.httpPort", json!(22));
    // And from the other side: SSH may not move onto the console's port.
    refused(&mut settings, "access.ssh.port", json!(8080));
    settings.set("mqtt.enabled", json!(true)).unwrap();
    refused(&mut settings, "access.web.httpPort", json!(1883));

    // A disabled HTTPS listener takes no port, and a disabled SSH none either.
    settings
        .set(
            "access.web",
            json!({ "httpPort": 8443, "httpsEnabled": false, "httpsPort": 8443 }),
        )
        .unwrap();
    settings.set("access.ssh.enabled", json!(false)).unwrap();
    settings.set("access.web.httpPort", json!(22)).unwrap();
}

/// `mode` is `network`, `local` or `off`, stored like the ports; anything
/// else is refused.
#[test]
pub(super) fn the_mode_is_one_of_three_words() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let mut settings = Settings::default();
    settings.set("access.web.mode", json!("local")).unwrap();
    store.save(&settings).unwrap();
    assert_eq!(
        config_document(&dir, SYSTEM_DOCUMENT)["web"]["mode"],
        json!("local")
    );
    assert_eq!(store.load().unwrap().access.web.mode, WebMode::Local);
    settings.set("access.web.mode", json!("off")).unwrap();
    assert_eq!(settings.access.web.mode, WebMode::Off);
    assert!(settings.set("access.web.mode", json!("public")).is_err());
}

/// Out of `network` apid binds no TCP port, so its ports claim none.
#[test]
pub(super) fn a_web_mode_without_the_network_takes_no_port() {
    let mut settings = Settings::default();
    settings.set("access.ssh.enabled", json!(true)).unwrap();
    settings.set("access.web.mode", json!("local")).unwrap();
    settings.set("access.web.httpPort", json!(22)).unwrap();
    let err = settings
        .set("access.web.mode", json!("network"))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("22 is already access.web.httpPort"),
        "{err}"
    );
}
