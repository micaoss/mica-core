//! The configuration documents: one per subtree, fail-closed, written 0600.

use micad_settings::{
    ApMode, CONFIG_DOCUMENTS, DEFAULT_CONFIG_DIR, DOCUMENT_MODE, MQTT_DOCUMENT, Settings,
    SettingsError, Store, WIFI_DOCUMENT, WIFI_SCHEMA_VERSION, WebAdminSettings, configuration,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::*;

/// Write raw bytes as one `/mica/config/` document, bypassing the store.
///
/// Every fixture here is written by hand rather than produced by a `save`,
/// because what is under test is what the reader does with a document this
/// build did not write.
pub(super) fn write_config(dir: &tempfile::TempDir, name: &str, text: &str) {
    fs::write(dir.path().join("config").join(name), text).unwrap();
}

/// Every document one device holds, `/mica/config/` and STATE alike.
pub(super) fn document_paths(dir: &tempfile::TempDir) -> Vec<PathBuf> {
    CONFIG_DOCUMENTS
        .iter()
        .map(|name| dir.path().join("config").join(name))
        .chain([dir.path().join("settings.toml")])
        .collect()
}

/// Bytes and inode of every document, keyed by file name.
///
/// The inode is half the claim. "The others are byte-identical" is what a
/// reader cares about; "the others were not rewritten at all" is what the
/// store actually promises, and only the inode separates them.
pub(super) fn fingerprints(dir: &tempfile::TempDir) -> BTreeMap<String, (Vec<u8>, u64)> {
    document_paths(dir)
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = fs::read(&path).unwrap();
            let ino = fs::metadata(&path).unwrap().ino();
            (name, (bytes, ino))
        })
        .collect()
}

pub(super) fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A device with something set in every one of the eight documents, so that a
/// test about one document's loss can see the other seven survive.
pub(super) fn configured() -> Settings {
    let mut settings = populated();
    settings.hostname = "edge-42".to_string();
    settings.access.console.shell_enabled = !settings.access.console.shell_enabled;
    settings.access.ssh.enabled = true;
    settings.access.web_admin = Some(WebAdminSettings {
        password_hash: "$argon2id$v=19$m=19456,t=2,p=1$ZGV2$ZGV2".to_string(),
    });
    settings.wifi.ap.mode = ApMode::Always;
    settings.wifi.ap.ssid = Some("mica-ap".to_string());
    settings.mqtt.enabled = true;
    settings.time.timezone = "Europe/Berlin".to_string();
    settings.container.enabled = !settings.container.enabled;
    settings.bluetooth.enabled = !settings.bluetooth.enabled;
    settings
}

/// **Fail-closed on a key the schema does not know, per document.**
#[test]
pub(super) fn a_document_with_an_unknown_key_fails_to_load_and_the_refusal_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);

    write_config(
        &dir,
        WIFI_DOCUMENT,
        &json!({
            "schema_version": WIFI_SCHEMA_VERSION,
            "wifi": { "ap": { "mode": "always", "enabld": true } },
        })
        .to_string(),
    );

    let err = store.load().unwrap_err();
    let SettingsError::Parse(message) = &err else {
        panic!("expected a parse error, got {err:?}");
    };
    assert!(
        message.contains("unknown field `enabld`"),
        "the refusal must name the offending key: {message}"
    );
    assert!(
        message.starts_with(WIFI_DOCUMENT),
        "the refusal must name the document it came from: {message}"
    );

    // The same document without the typo loads, so what was refused is the key
    // and not the fixture.
    write_config(
        &dir,
        WIFI_DOCUMENT,
        &json!({
            "schema_version": WIFI_SCHEMA_VERSION,
            "wifi": { "ap": { "mode": "always" } },
        })
        .to_string(),
    );
    assert_eq!(store.load().unwrap().wifi.ap.mode, ApMode::Always);
}

/// The reason the other six share, asserted rather than argued.
#[test]
pub(super) fn every_config_document_is_fail_closed_on_an_unknown_key() {
    for document in CONFIG_DOCUMENTS {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(&dir);
        write_config(&dir, document, r#"{"schema_version": 1, "bogus": true}"#);

        let err = store.load().unwrap_err();
        let SettingsError::Parse(message) = &err else {
            panic!("{document}: expected a parse error, got {err:?}");
        };
        assert!(
            message.contains("unknown field `bogus`"),
            "{document}: {message}"
        );
        assert!(
            message.starts_with(document),
            "{document}: the refusal must name the document: {message}"
        );
    }
}

/// **A document that exists and does not parse is a refusal, not a default.**
#[test]
pub(super) fn a_document_that_does_not_parse_refuses_the_load_and_names_itself() {
    for (text, needle) in [
        // Not JSON at all. The wording after the document name is serde_json's
        // and is not asserted; that it is a refusal and names `mqtt.json` is.
        ("{ this is not json", None),
        // Parses, but carries no version stamp, so nothing says which schema
        // it is written against.
        (r#"{"mqtt": {"enabled": true}}"#, Some("no schema_version")),
        // A stamp that is not an integer.
        (
            r#"{"schema_version": "1", "mqtt": {}}"#,
            Some("schema_version must be an integer"),
        ),
        // A document whose top level is not a table at all.
        ("[]", Some("the top level is not a table")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(&dir);
        write_config(&dir, MQTT_DOCUMENT, text);

        let err = store.load().unwrap_err();
        let SettingsError::Parse(message) = &err else {
            panic!("{text}: expected a parse error, got {err:?}");
        };
        assert!(
            message.starts_with(MQTT_DOCUMENT),
            "{text}: the refusal must name the document: {message}"
        );
        if let Some(needle) = needle {
            assert!(message.contains(needle), "{text}: {message}");
        }

        // And the contrast that makes the refusal mean something: remove the
        // file and the very same store loads, because absence IS a default.
        fs::remove_file(dir.path().join("config").join(MQTT_DOCUMENT)).unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
    }
}

/// A document from a build one schema version behind this one.
#[test]
pub(super) fn an_older_document_is_refused_by_name_because_there_is_no_migration() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    write_config(&dir, MQTT_DOCUMENT, r#"{"schema_version": 0, "mqtt": {}}"#);

    let err = store.load().unwrap_err();
    let SettingsError::SchemaVersion(message) = &err else {
        panic!("expected a schema version error, got {err:?}");
    };
    assert!(message.starts_with(MQTT_DOCUMENT), "{message}");
    assert!(message.contains("this build requires"), "{message}");
}

/// A future schema that must be refused without rewriting its fields.
pub(super) fn newer_wifi_document() -> String {
    json!({
        "schema_version": WIFI_SCHEMA_VERSION + 1,
        "wifi": {
            "ap": { "mode": "always", "ssid": "mica-ap", "channel": 11, "band": "6ghz" },
            "client": {
                "enabled": true,
                "interface": "wlan0",
                "networks": [{ "ssid": "site", "psk": "hunter2hunter2", "priority": 3 }],
            },
        },
    })
    .to_string()
}

/// **A key written to one document leaves the others byte-identical** — the
/// claim the split is made of, checked on the inodes as well as on the bytes:
/// an unchanged document is not rewritten at all.
#[test]
pub(super) fn a_write_to_one_document_leaves_the_others_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let settings = configured();
    store.save(&settings).unwrap();
    let before = fingerprints(&dir);

    let changed = Settings {
        mqtt: micad_settings::MqttSettings {
            enabled: !settings.mqtt.enabled,
            ..settings.mqtt.clone()
        },
        ..settings
    };
    store.save(&changed).unwrap();

    let after = fingerprints(&dir);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (name, fingerprint) in &before {
        if name == MQTT_DOCUMENT {
            assert_ne!(
                &after[name], fingerprint,
                "the document that was written must have changed"
            );
        } else {
            assert_eq!(
                &after[name], fingerprint,
                "{name} must be byte-identical, inode included"
            );
        }
    }
    assert_eq!(store.load().unwrap(), changed);
}

/// **Every document is written `0600`, including over a laxer one.**
///
/// The namespace is credential material — `wifi.json` carries the site's WPA2
/// pre-shared key — so a document reachable under its final name at `0644` has
/// already published it.
#[test]
pub(super) fn every_document_is_written_0600_including_over_a_laxer_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    store.save(&Settings::default()).unwrap();

    let paths = document_paths(&dir);
    assert_eq!(paths.len(), CONFIG_DOCUMENTS.len() + 1);
    for path in &paths {
        assert!(path.exists(), "{} was not written", path.display());
        assert_eq!(mode_of(path), DOCUMENT_MODE, "{}", path.display());
    }

    // Publish every one of them, then write a settings tree that changes all
    // eight — an unchanged document is deliberately not rewritten, so a
    // narrower edit would leave most of them at 0666 for a reason that is not
    // this test's subject.
    for path in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(mode_of(path), 0o666);
    }
    store.save(&configured()).unwrap();

    for path in &paths {
        assert_eq!(
            mode_of(path),
            DOCUMENT_MODE,
            "{} kept the mode of the file it replaced",
            path.display()
        );
    }
}

/// **A namespace that is not there is a refusal, and the refusal names the
/// mount.** Both directions: nothing is loaded on schema defaults and nothing
/// is written.
#[test]
pub(super) fn a_missing_configuration_namespace_refuses_and_names_the_mount() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("settings.toml");
    // Deliberately NOT created: this is the DATA medium being gone, not a
    // document that was never written.
    let store = Store::new(&state, dir.path().join("mica").join("config"));

    for err in [
        store.load().unwrap_err(),
        store.save(&Settings::default()).unwrap_err(),
    ] {
        let SettingsError::Unavailable { directory, mount } = &err else {
            panic!("expected an unavailable error, got {err:?}");
        };
        assert!(directory.ends_with("/mica/config"), "{directory}");
        assert!(mount.ends_with("/mica"), "{mount}");
        let message = err.to_string();
        assert!(
            message.contains(mount) && message.contains("not mounted"),
            "the refusal must name the mount an operator has to fix: {message}"
        );
    }
    assert!(!state.exists(), "a refused save must write nothing");
}

/// The two spellings of one directory agree.
#[test]
pub(super) fn the_update_document_lives_inside_the_configuration_namespace() {
    assert!(
        configuration::DEFAULT_UPDATES_PATH.starts_with(&format!("{DEFAULT_CONFIG_DIR}/")),
        "{} is not inside {DEFAULT_CONFIG_DIR}",
        configuration::DEFAULT_UPDATES_PATH
    );
}

// --- The pour --------------------------------------------
