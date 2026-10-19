//! Refused documents: adopted, preserved and named.

use micad_settings::{
    CONFIG_DOCUMENTS, CONTAINER_DOCUMENT, MQTT_DOCUMENT, NETWORK_DOCUMENT, SSH_DOCUMENT,
    SYSTEM_DOCUMENT, Settings, SettingsError, Store, TIME_DOCUMENT, WIFI_DOCUMENT,
    WIFI_SCHEMA_VERSION, configuration, document_subtrees,
};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;

/// A hand-written, valid document per reconciler, with the value that proves
/// it reached the addressed tree.
pub(super) fn poured_namespace() -> Vec<Poured> {
    let poured = |document, text: String, adopted: fn(&Settings) -> bool| Poured {
        document,
        text,
        adopted,
    };
    vec![
        poured(
            SYSTEM_DOCUMENT,
            r#"{"schema_version": 1, "hostname": "poured-edge-1"}"#.to_string(),
            |settings| settings.hostname == "poured-edge-1",
        ),
        poured(
            NETWORK_DOCUMENT,
            r#"{"schema_version": 1, "network": {"eth0": {"dhcp": false}}}"#.to_string(),
            |settings| settings.network.get("eth0").is_some_and(|eth| !eth.dhcp),
        ),
        poured(
            WIFI_DOCUMENT,
            format!(
                r#"{{"schema_version": 1, "wifi": {{"ap": {{"mode": "always", "ssid": "poured-ap", "psk": "{POURED_PSK}"}}}}}}"#
            ),
            |settings| settings.wifi.ap.ssid.as_deref() == Some("poured-ap"),
        ),
        poured(
            SSH_DOCUMENT,
            r#"{"schema_version": 1, "ssh": {"enabled": true, "port": 2222}}"#.to_string(),
            |settings| settings.access.ssh.port == 2222,
        ),
        poured(
            MQTT_DOCUMENT,
            r#"{"schema_version": 1, "mqtt": {"enabled": true}}"#.to_string(),
            |settings| settings.mqtt.enabled,
        ),
        poured(
            TIME_DOCUMENT,
            r#"{"schema_version": 1, "time": {"timezone": "Europe/Berlin"}}"#.to_string(),
            |settings| settings.time.timezone == "Europe/Berlin",
        ),
        poured(
            CONTAINER_DOCUMENT,
            r#"{"schema_version": 1, "container": {"enabled": true}}"#.to_string(),
            |settings| settings.container.enabled,
        ),
    ]
}

/// One hand-written document, and the reading that proves it was adopted.
pub(super) struct Poured {
    pub(super) document: &'static str,
    pub(super) text: String,
    pub(super) adopted: fn(&Settings) -> bool,
}

/// The site key an integrator pours into `wifi.json`.
///
/// A value with no other reason to appear anywhere, so a test that greps a
/// served record for it is asserting about this key and not about a word that
/// happens to be common.
pub(super) const POURED_PSK: &str = "poured-site-key-9d4ec7b0";

/// Pour the whole namespace by hand and assert every document is adopted.
#[test]
pub(super) fn a_hand_written_namespace_is_adopted_whole() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    for entry in poured_namespace() {
        write_config(&dir, entry.document, &entry.text);
    }

    let loaded = store.load_with_refusals().unwrap();

    assert!(
        loaded.refusals.is_empty(),
        "a valid pour refuses nothing: {:?}",
        loaded.refusals
    );
    for entry in poured_namespace() {
        assert!(
            (entry.adopted)(&loaded.settings),
            "{} parsed and validated and was not adopted",
            entry.document
        );
    }
    // Not written back on the way through: the pour is a read, and a loader
    // that normalised what it found would have rewritten the integrator's file
    // before anybody could look at it.
    assert_eq!(
        fs::read_to_string(dir.path().join("config").join(SYSTEM_DOCUMENT)).unwrap(),
        r#"{"schema_version": 1, "hostname": "poured-edge-1"}"#
    );
}

/// **Clause 2, both directions, for every document in the namespace.**
#[test]
pub(super) fn one_unparseable_document_refuses_itself_and_nothing_else() {
    for broken in poured_namespace().iter().map(|entry| entry.document) {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(&dir);
        for entry in poured_namespace() {
            if entry.document == broken {
                write_config(
                    &dir,
                    entry.document,
                    r#"{"schema_version": 1, "typo": true}"#,
                );
            } else {
                write_config(&dir, entry.document, &entry.text);
            }
        }

        let loaded = store.load_with_refusals().unwrap();

        let names: Vec<&str> = loaded
            .refusals
            .iter()
            .map(|refusal| refusal.document.as_str())
            .collect();
        assert_eq!(names, vec![broken], "exactly one document is refused");

        let refusal = &loaded.refusals[0];
        // Names the file, which the gate requires in as many words: the person
        // who poured it has no other way to learn which one it was.
        assert!(
            refusal
                .message
                .contains(&refusal.path.display().to_string()),
            "the refusal must name the file: {}",
            refusal.message
        );
        assert!(refusal.path.ends_with(broken), "{:?}", refusal.path);
        assert!(
            refusal.detail.contains("unknown field `typo`"),
            "{}",
            refusal.detail
        );
        assert_eq!(refusal.subtrees, document_subtrees(broken));
        assert!(!refusal.subtrees.is_empty());

        // The other six are adopted, from the same load.
        for entry in poured_namespace() {
            if entry.document == broken {
                continue;
            }
            assert!(
                (entry.adopted)(&loaded.settings),
                "{broken} was refused and took {} down with it",
                entry.document
            );
        }
    }
}

/// **A refusal is not a schema default with a log line.**
#[test]
pub(super) fn a_refused_document_is_distinguishable_from_an_absent_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    write_config(&dir, WIFI_DOCUMENT, "{ not json");

    let refused = store.load_with_refusals().unwrap();
    assert_eq!(refused.refusals.len(), 1);
    assert_eq!(refused.settings.wifi, Settings::default().wifi);

    fs::remove_file(dir.path().join("config").join(WIFI_DOCUMENT)).unwrap();
    let absent = store.load_with_refusals().unwrap();
    assert!(absent.refusals.is_empty());

    assert_eq!(refused.settings, absent.settings);
}

/// Every way a `/mica/config/` document can fail reaches the same refusal.
#[test]
pub(super) fn every_way_a_document_fails_is_a_refusal_and_not_a_default() {
    for text in [
        "{ this is not json",
        r#"{"mqtt": {"enabled": true}}"#,
        r#"{"schema_version": "1", "mqtt": {}}"#,
        "[]",
        r#"{"schema_version": 0, "mqtt": {}}"#,
        r#"{"schema_version": 1, "mqtt": {"enabld": true}}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(&dir);
        write_config(&dir, MQTT_DOCUMENT, text);

        let loaded = store.load_with_refusals().unwrap();
        assert_eq!(
            loaded.refusals.len(),
            1,
            "{text}: expected one refusal, got {:?}",
            loaded.refusals
        );
        assert_eq!(loaded.refusals[0].document, MQTT_DOCUMENT);
        assert!(
            loaded.refusals[0]
                .message
                .contains(&loaded.refusals[0].path.display().to_string()),
            "{text}: {}",
            loaded.refusals[0].message
        );
        assert!(!loaded.settings.mqtt.enabled, "{text}: adopted anyway");
    }

    // A document the process cannot read. Skipped when the tests run as root,
    // which ignores the mode -- and that is stated rather than silently
    // passing, because a check that cannot fail is not a check.
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let path = dir.path().join("config").join(MQTT_DOCUMENT);
    write_config(&dir, MQTT_DOCUMENT, r#"{"schema_version": 1, "mqtt": {}}"#);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_to_string(&path).is_ok() {
        eprintln!("running as root: the unreadable-document leg asserts nothing here");
    } else {
        let loaded = store.load_with_refusals().unwrap();
        assert_eq!(loaded.refusals.len(), 1);
        assert_eq!(loaded.refusals[0].document, MQTT_DOCUMENT);
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// **The two rules the pour does NOT relax.**
#[test]
pub(super) fn the_medium_and_the_state_document_still_refuse_the_whole_load() {
    let dir = tempfile::tempdir().unwrap();
    let absent = Store::new(dir.path().join("settings.toml"), dir.path().join("config"));
    assert!(matches!(
        absent.load_with_refusals().unwrap_err(),
        SettingsError::Unavailable { .. }
    ));

    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    fs::write(dir.path().join("settings.toml"), "this is not toml = [").unwrap();
    assert!(matches!(
        store.load_with_refusals().unwrap_err(),
        SettingsError::Parse(_)
    ));
}

/// **A later save must not overwrite a refused document with the default it
/// was refused in favour of.**
#[test]
pub(super) fn a_save_leaves_a_preserved_document_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let broken = r#"{"schema_version": 1, "wifi": {"ap": {"mode": "alwys"}}}"#;
    write_config(&dir, WIFI_DOCUMENT, broken);

    let loaded = store.load_with_refusals().unwrap();
    assert_eq!(loaded.refusals.len(), 1);
    let guarded = store.preserving(&[WIFI_DOCUMENT]);

    let mut settings = loaded.settings;
    settings.hostname = "edge-1".to_string();
    guarded.save(&settings).unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("config").join(WIFI_DOCUMENT)).unwrap(),
        broken,
        "the refused document must not be rewritten from a schema default"
    );
    assert_eq!(
        config_document(&dir, SYSTEM_DOCUMENT)["hostname"],
        json!("edge-1"),
        "the write the operator asked for must still land"
    );

    // And the repair: the same write through a store that does NOT preserve it
    // is the authenticated edit, and it replaces the bytes.
    settings.wifi.ap.ssid = Some("repaired".to_string());
    store.save(&settings).unwrap();
    assert_eq!(
        config_document(&dir, WIFI_DOCUMENT)["wifi"]["ap"]["ssid"],
        json!("repaired")
    );
    assert!(store.load_with_refusals().unwrap().refusals.is_empty());
}

/// A preserved name whose file is gone is written.
///
/// Tier 1 empties `/mica/config/` and then saves the re-seeded tree
/// (`reset.rs`). If preservation were by name alone, the refused document would
/// be the one occupant a factory reset failed to restore — and the namespace
/// would come back one document short of what the reset is defined to produce.
#[test]
pub(super) fn a_preserved_document_that_no_longer_exists_is_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir).preserving(&[WIFI_DOCUMENT]);
    write_config(
        &dir,
        WIFI_DOCUMENT,
        r#"{"schema_version": 1, "typo": true}"#,
    );

    store.save(&Settings::default()).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("config").join(WIFI_DOCUMENT)).unwrap(),
        r#"{"schema_version": 1, "typo": true}"#
    );

    fs::remove_file(dir.path().join("config").join(WIFI_DOCUMENT)).unwrap();
    store.save(&Settings::default()).unwrap();
    assert_eq!(
        config_document(&dir, WIFI_DOCUMENT)["schema_version"],
        json!(WIFI_SCHEMA_VERSION)
    );
}

/// A plain store preserves nothing, asserted rather than assumed: every
/// existing caller goes through it and the pour must not have changed what they
/// do.
#[test]
pub(super) fn a_store_preserves_nothing_unless_it_was_narrowed() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    write_config(
        &dir,
        WIFI_DOCUMENT,
        r#"{"schema_version": 1, "typo": true}"#,
    );

    let settings = Settings {
        hostname: "edge-1".to_string(),
        ..Settings::default()
    };
    store.save(&settings).unwrap();

    assert_eq!(
        config_document(&dir, WIFI_DOCUMENT)["schema_version"],
        json!(WIFI_SCHEMA_VERSION)
    );
    assert!(store.load_with_refusals().unwrap().refusals.is_empty());
}

/// **Every occupant of `/mica/config/` fails closed, and every refusal names its
/// file.**
#[test]
pub(super) fn every_occupant_of_the_namespace_fails_closed_and_names_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    for document in CONFIG_DOCUMENTS {
        write_config(&dir, document, "{ not a document");
    }
    let loaded = store.load_with_refusals().unwrap();
    let mut refused: Vec<&str> = loaded
        .refusals
        .iter()
        .map(|refusal| refusal.document.as_str())
        .collect();
    refused.sort_unstable();
    let mut expected: Vec<&str> = CONFIG_DOCUMENTS.to_vec();
    expected.sort_unstable();
    assert_eq!(refused, expected, "every document refuses on its own terms");
    for refusal in &loaded.refusals {
        assert!(
            refusal
                .message
                .contains(&refusal.path.display().to_string()),
            "{}",
            refusal.message
        );
    }
    // Nothing was adopted, and nothing was invented either: the tree is the
    // schema default, which is the only total value there is — and the refusal
    // list beside it is what stops that from being read as configuration.
    assert_eq!(loaded.settings, Settings::default());

    // The eighth occupant, through the other reader. Its refusal is an `Err`
    // rather than an entry, because the update policy is not part of the
    // addressed tree; what it shares is that it names the file and that it
    // never resolves to a value (the `LoadedPolicy` carries the error
    // beside a policy with no selection at all).
    let updates = dir.path().join("config").join("updates.json");
    fs::write(&updates, "{ not a document").unwrap();
    let err = configuration::load_updates(&updates).unwrap_err();
    assert!(
        err.to_string().contains(&updates.display().to_string()),
        "the update document's refusal must name the file too: {err}"
    );
}

/// **A served refusal names the file and quotes nothing the document carried.**
#[test]
pub(super) fn a_refusal_names_the_file_and_quotes_nothing_the_document_carried() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    write_config(
        &dir,
        WIFI_DOCUMENT,
        &format!(r#"{{"schema_version": 1, "wifi": {{"ap": {{"channel": "{POURED_PSK}"}}}}}}"#),
    );

    let loaded = store.load_with_refusals().unwrap();
    assert_eq!(loaded.refusals.len(), 1);
    let refusal = &loaded.refusals[0];

    assert!(
        refusal
            .message
            .contains(&refusal.path.display().to_string()),
        "{}",
        refusal.message
    );
    assert!(
        refusal.message.contains("did not parse"),
        "{}",
        refusal.message
    );
    assert!(
        !refusal.message.contains(POURED_PSK),
        "the served refusal quoted the document: {}",
        refusal.message
    );
    assert!(
        refusal.detail.contains(POURED_PSK),
        "the fixture must actually reproduce the hazard, or the assertion above \
         asserts nothing: {}",
        refusal.detail
    );
}

/// The three classes a served refusal may say, each from its own trigger.
///
/// Chosen by this build rather than supplied by the parser, so the set is
/// closed: a reader can act on all three — fix the bytes, fix the version, fix
/// the permissions — and none of them can carry a byte of the document.
#[test]
pub(super) fn a_served_refusal_says_which_of_three_things_went_wrong() {
    for (text, class) in [
        (
            r#"{"schema_version": 1, "mqtt": {"typo": true}}"#,
            "did not parse",
        ),
        (
            r#"{"schema_version": 0, "mqtt": {}}"#,
            "has an unsupported schema version",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(&dir);
        write_config(&dir, MQTT_DOCUMENT, text);
        let loaded = store.load_with_refusals().unwrap();
        assert_eq!(loaded.refusals.len(), 1, "{text}");
        assert!(
            loaded.refusals[0].message.contains(class),
            "{text}: {}",
            loaded.refusals[0].message
        );
    }
}

#[test]
pub(super) fn rejects_future_settings_without_stripping_or_defaulting_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let original = newer_wifi_document();
    write_config(&dir, WIFI_DOCUMENT, &original);
    assert!(store.load().is_err(), "future schema was silently adopted");
    let loaded = store.load_with_refusals().unwrap();
    assert_eq!(loaded.refusals.len(), 1);
    assert_eq!(loaded.refusals[0].document, WIFI_DOCUMENT);
    assert_eq!(
        fs::read_to_string(store.config_dir().join(WIFI_DOCUMENT)).unwrap(),
        original
    );
}

// --- Declared containers -----------------------------------------------------
