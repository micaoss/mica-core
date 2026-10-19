//! Invalid documents and what never leaks from them.

use micad_settings::Settings;
use std::fs;
use tempfile::TempDir;

use super::*;

/// Every way a document can be wrong, with the key path its refusal must
/// name. Driven as one table so a new rule is one row.
pub(super) fn invalid_documents() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "no version field",
            "[time]\ntimezone = \"UTC\"\n".to_string(),
            "version",
        ),
        (
            "a version this build does not apply",
            "version = 2\n".to_string(),
            "version",
        ),
        (
            "a version that is not an integer",
            "version = \"1\"\n".to_string(),
            "version",
        ),
        (
            "a section the schema does not have",
            "version = 1\n\n[certificates]\nca = \"x\"\n".to_string(),
            "certificates",
        ),
        (
            "a misspelled key inside a section",
            "version = 1\n\n[identity]\ndeviceID = \"0123456789abcdef0123456789abcdef\"\n"
                .to_string(),
            "identity.deviceID",
        ),
        (
            "a device identifier that is not 32 lowercase hex",
            "version = 1\n\n[identity]\ndeviceId = \"0123456789ABCDEF0123456789abcdef\"\n"
                .to_string(),
            "identity.deviceId",
        ),
        (
            "a device identifier of the wrong length",
            "version = 1\n\n[identity]\ndeviceId = \"abc\"\n".to_string(),
            "identity.deviceId",
        ),
        (
            "an administrator password below the floor",
            "version = 1\n\n[admin]\npassword = \"short\"\n".to_string(),
            "admin.password",
        ),
        (
            "an authorized key that is not one",
            "version = 1\n\n[admin]\nauthorizedKeys = [\"command=/bin/sh ssh-ed25519 AAAA\"]\n"
                .to_string(),
            "admin.authorizedKeys[0]",
        ),
        (
            "an interface name that is not one",
            "version = 1\n\n[network.\"eth 0\"]\ndhcp = true\n".to_string(),
            "network",
        ),
        (
            "a network entry of the wrong shape",
            "version = 1\n\n[network.eth0]\ndhcp = \"yes\"\n".to_string(),
            "network",
        ),
        (
            "a WiFi network with no name",
            "version = 1\n\n[[wifi.networks]]\nssid = \"\"\n".to_string(),
            "wifi.networks[0].ssid",
        ),
        (
            "a pre-shared key no supplicant could use",
            format!(
                "version = 1\n\n[[wifi.networks]]\nssid = \"s\"\npsk = \"{}\"\n",
                unusable_psk()
            ),
            "wifi.networks[0].psk",
        ),
        (
            "an NTP server that could smuggle a second assignment",
            "version = 1\n\n[time.ntp]\nservers = [\"pool one\"]\n".to_string(),
            "time.ntp.servers",
        ),
        (
            "a timezone that is not an IANA zone name",
            "version = 1\n\n[time]\ntimezone = \"../etc/passwd\"\n".to_string(),
            "time.timezone",
        ),
    ]
}

// The acceptance criterion, stated twice over: one bad field applies
// NOTHING, and the refusal names the offending KEY PATH.
#[test]
pub(super) fn a_document_with_one_bad_field_applies_nothing_and_names_the_key() {
    for (label, body, key) in invalid_documents() {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let root = stage(dir.path(), Source::Boot, &body);
        let mut settings = Settings::default();

        let outcome = import(&store, &mut settings, &root).expect("import");
        let Outcome::Rejected { source, rejection } = outcome else {
            panic!("{label}: expected a rejection, got {outcome:?}");
        };
        assert_eq!(source, Source::Boot);
        assert_eq!(
            rejection.key, key,
            "{label}: wrong key path in {rejection:?}"
        );
        assert!(!rejection.reason.is_empty(), "{label}: no reason given");

        let mut without_record = settings.clone();
        without_record.provisioning.document = None;
        assert_eq!(
            without_record,
            Settings::default(),
            "{label}: a refused document moved something"
        );

        let record = settings
            .provisioning
            .document
            .as_ref()
            .expect("a record of the refusal");
        assert_eq!(record.applied_version, None);
        assert_eq!(record.applied_digest, None);
        let attempt = record.last_import.as_ref().expect("an import record");
        assert_eq!(attempt.outcome, "rejected");
        let reason = attempt.reason.as_deref().expect("a reason");
        assert!(
            reason.contains(key),
            "{label}: the recorded reason must name the key, got {reason:?}"
        );

        // And the device is still a working, UNCLAIMED appliance: nothing
        // about a refused document may take it out of setup mode.
        assert!(settings.access.web_admin.is_none(), "{label}");
        assert_eq!(store.load().expect("reload"), settings, "{label}");
    }
}

// The secret-safety acceptance criterion. A document carrying two known
// sentinels is driven through the WHOLE path — apply, record, re-import,
// refuse — and neither sentinel may appear in anything emitted.
#[test]
pub(super) fn no_secret_from_the_document_reaches_a_report_or_the_record() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Boot, &full_document());
    let mut settings = Settings::default();

    let applied = import(&store, &mut settings, &root).expect("import");
    let mut emitted = vec![format!("{applied:?}")];

    // The subtree the status route reads and `GetSettings("provisioning")`
    // serves, in the form it is served in.
    emitted.push(serde_json::to_string(&settings.provisioning).expect("the subtree serializes"));
    emitted.push(toml::to_string(&settings.provisioning).expect("the subtree renders"));

    // The re-import, and the refusal a claimed device gives a DIFFERENT
    // document carrying the same secrets.
    let mut reloaded = store.load().expect("reload");
    emitted.push(format!(
        "{:?}",
        import(&store, &mut reloaded, &root).expect("re-import")
    ));
    let altered = full_document().replace("priority = 10", "priority = 11");
    let root = stage(dir.path(), Source::Media, &altered);
    // The boot document is still staged, so remove it: this is about what
    // the media path reports.
    fs::remove_file(root.join(Source::Boot.dir_name()).join(DOCUMENT_FILE_NAME))
        .expect("remove the boot document");
    let mut reloaded = store.load().expect("reload");
    let refused = import(&store, &mut reloaded, &root).expect("refused import");
    assert!(
        matches!(refused, Outcome::Rejected { .. }),
        "a claimed device must refuse a new document, got {refused:?}"
    );
    emitted.push(format!("{refused:?}"));
    emitted.push(serde_json::to_string(&reloaded.provisioning).expect("serializes"));

    // Every rejection this module can raise about a secret-bearing key.
    for body in [
        format!(
            "version = 1\n\n[admin]\npassword = \"{}\"\n",
            &SECRET_PASSWORD[..4]
        ),
        format!(
            "version = 1\n\n[[wifi.networks]]\nssid = \"s\"\npsk = \"{}\"\n",
            unusable_psk()
        ),
        format!("version = 1\n\n[admin]\npassword = {SECRET_PASSWORD:?}\nbogus = 1\n"),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let root = stage(dir.path(), Source::Media, &body);
        let mut fresh = Settings::default();
        let outcome = import(&store, &mut fresh, &root).expect("import");
        emitted.push(format!("{outcome:?}"));
        if let Outcome::Rejected { rejection, .. } = &outcome {
            emitted.push(rejection.to_string());
        }
        emitted.push(serde_json::to_string(&fresh.provisioning).expect("serializes"));
    }

    for text in &emitted {
        for sentinel in [SECRET_PASSWORD, SECRET_PSK, &SECRET_PASSWORD[..4]] {
            assert!(
                !text.contains(sentinel),
                "a document secret reached an emitted string: {sentinel:?} in {text:?}"
            );
        }
    }
    // The search space is populated: a scan over empty strings would pass
    // forever.
    assert!(emitted.len() >= 10, "{emitted:?}");
    assert!(
        emitted.iter().any(|text| text.contains("rejected")),
        "no refusal was emitted, so the refusal half proves nothing: {emitted:?}"
    );
}

// The stored hash is not the password, which is the other half of the
// claim above: the plaintext is dropped, not moved.
#[test]
pub(super) fn the_bootstrap_password_is_stored_only_as_a_hash() {
    let dir = TempDir::new().expect("tempdir");
    let (_, settings, _) = import_body(dir.path(), Source::Boot, &full_document());
    let rendered = toml::to_string(&settings).expect("the tree renders");
    assert!(
        !rendered.contains(SECRET_PASSWORD),
        "the plaintext password reached the settings file"
    );
    assert!(rendered.contains("$argon2id$"));
}
