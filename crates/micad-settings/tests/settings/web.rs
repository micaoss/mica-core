//! Where apid listens: `access.web`.

use micad_settings::{SYSTEM_DOCUMENT, Settings, SettingsError, WebSettings};
use serde_json::json;

use super::{config_document, store_at};

/// Off the well-known ports: plain HTTP on 8080, HTTPS off and on 8443 when
/// it is turned on.
#[test]
pub(super) fn the_console_listens_on_8080_by_default() {
    assert_eq!(
        Settings::default().access.web,
        WebSettings {
            http_port: 8080,
            https_enabled: false,
            https_port: 8443,
        }
    );
}

/// `system.json` carries `web` only once it is not the default, so a device
/// that never moved its console writes the document an older micad reads.
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
        json!({ "httpPort": 8081, "httpsEnabled": true, "httpsPort": 9443 })
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
