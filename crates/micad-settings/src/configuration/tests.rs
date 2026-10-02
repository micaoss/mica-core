#[test]
fn fleet_document_rejects_explicit_null_booleans() {
    for document in [
        r#"{ "schema": "mica/fleet-config/v1", "enabled": null }"#,
        r#"{ "schema": "mica/fleet-config/v1", "reporting": null }"#,
    ] {
        assert!(serde_json::from_str::<super::FleetDocument>(document).is_err());
    }
}

#[test]
fn fleet_urls_require_complete_https_authorities_without_normalizing_location() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.json");
    for url in [
        "https://fleet.example.invalid/path?mode=test#fragment",
        "https://192.0.2.1:8443/report",
        "https://[2001:db8::1]/report?device=1",
    ] {
        std::fs::write(
            &path,
            json!({ "schema": FLEET_SCHEMA_TAG, "url": url }).to_string(),
        )
        .unwrap();
        assert_eq!(
            load_fleet(&path).unwrap().unwrap().url,
            Some(Some(url.to_string()))
        );
    }

    for url in [
        "https://bad host/REJECTED-FLEET-SENTINEL",
        "https://[]/REJECTED-FLEET-SENTINEL",
        "https://REJECTED-FLEET-SENTINEL@fleet.example/path",
    ] {
        std::fs::write(
            &path,
            json!({ "schema": FLEET_SCHEMA_TAG, "url": url }).to_string(),
        )
        .unwrap();
        let error = load_fleet(&path).unwrap_err().to_string();
        assert!(!error.contains("REJECTED-FLEET-SENTINEL"));
    }
}

#[test]
fn fleet_resolution_applies_baked_fallbacks_and_the_reporting_gate() {
    let baked = super::BakedFleet {
        enabled: true,
        url: Some("https://baked.example/fleet".to_string()),
    };
    assert_eq!(
        super::effective_fleet(&baked, None),
        super::EffectiveFleet {
            enabled: true,
            reporting: true,
            url: Some("https://baked.example/fleet".to_string()),
        }
    );

    let disabled = super::FleetDocument {
        schema: super::FLEET_SCHEMA_TAG.to_string(),
        enabled: Some(false),
        reporting: Some(true),
        url: Some(None),
    };
    assert_eq!(
        super::effective_fleet(&baked, Some(&disabled)),
        super::EffectiveFleet {
            enabled: false,
            reporting: false,
            url: Some("https://baked.example/fleet".to_string()),
        }
    );
}

/// The baked defaults: v1 loads; an unknown key, such as `channel`, or
/// another schema tag is refused.
#[test]
fn a_baked_manifest_refuses_an_unknown_key_or_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    let source = "https://baked.example/update/";
    let document = |schema: &str, update: Value| {
        json!({
            "schema": schema,
            "product": { "vendor": "example", "model": "mica-appliance" },
            "update": update,
            "http": { "credentialHosts": [] },
            "fleet": { "enabled": false, "url": null },
        })
        .to_string()
    };
    let current = json!({ "source": source, "policy": "check", "checkIntervalMinutes": 60 });
    let mut with_channel = current.clone();
    with_channel["channel"] = json!("stable");

    std::fs::write(&path, document(MANIFEST_SCHEMA_TAG, current.clone())).unwrap();
    let loaded = load_manifest(&path);
    assert!(loaded.error.is_none(), "{:?}", loaded.error);
    assert_eq!(loaded.manifest.update.source.as_deref(), Some(source));
    assert_eq!(loaded.manifest.update.check_interval_minutes, 60);

    for refused in [
        document(MANIFEST_SCHEMA_TAG, with_channel),
        document("mica/meta/v2", current),
    ] {
        std::fs::write(&path, refused).unwrap();
        let loaded = load_manifest(&path);
        assert!(loaded.error.is_some());
        assert_eq!(
            loaded.manifest.update.source, None,
            "a refusal is the code defaults"
        );
    }
}

#[test]
fn baked_manifest_keeps_metadata_anchors_in_the_authenticated_boot_policy() {
    let mut value = serde_json::to_value(super::BakedManifest::code_defaults()).unwrap();
    value.as_object_mut().unwrap().remove("trust");
    assert!(serde_json::from_value::<super::BakedManifest>(value.clone()).is_ok());
    value["trust"] = serde_json::json!({"signingKeys": [], "signingKeyIds": []});
    assert!(serde_json::from_value::<super::BakedManifest>(value).is_err());
}

use std::os::unix::fs::PermissionsExt;

use super::*;
use chrono::DateTime;
use chrono::Utc;

#[test]
fn update_sources_reject_removed_metadata_path_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    for key in ["repoDir", "statePath"] {
        std::fs::write(&path, json!({"source":{key:"/tmp/metadata"}}).to_string()).unwrap();
        assert!(
            load_updates(&path).is_err(),
            "removed option accepted: {key}"
        );
    }
}

/// The anchor is a clock face, and the document says so before a device
/// schedules on it.
#[test]
fn the_check_anchor_must_be_a_clock_face() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    for accepted in ["00:00", "03:30", "23:59"] {
        std::fs::write(&path, json!({ "checkAt": accepted }).to_string()).unwrap();
        assert_eq!(
            load_updates(&path).unwrap().check_at,
            Some(Some(accepted.to_string()))
        );
    }
    for refused in ["3:00", "24:00", "03:60", "0300", "03:00:00"] {
        std::fs::write(&path, json!({ "checkAt": refused }).to_string()).unwrap();
        let error = load_updates(&path).unwrap_err().to_string();
        assert!(error.contains("is not HH:MM"), "{refused}: {error}");
    }
    // `null` is a value here as everywhere: the anchor is cleared, and
    // the device is back on its interval.
    std::fs::write(&path, json!({ "checkAt": null }).to_string()).unwrap();
    assert_eq!(load_updates(&path).unwrap().check_at, Some(None));
}

/// The anchor is layer 2's outright, like `rebootPolicy`: the baked layer
/// has no opinion to override.
#[test]
fn the_check_anchor_resolves_from_the_operator_document_alone() {
    let baked = BakedUpdate::code_defaults();
    let document = UpdatesDocument {
        check_at: Some(Some("03:00".to_string())),
        ..UpdatesDocument::default()
    };
    assert_eq!(
        resolve(&baked, document).check_at,
        Some("03:00".to_string())
    );
    assert_eq!(resolve(&baked, UpdatesDocument::default()).check_at, None);
}

/// The crossing is the most recent one at or before now, which is
/// yesterday's when today's has not come round yet.
#[test]
fn the_last_crossing_is_todays_or_yesterdays() {
    let now = DateTime::parse_from_rfc3339("2026-09-20T04:30:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        last_crossing("03:00", now).unwrap().to_rfc3339(),
        "2026-09-20T03:00:00+00:00"
    );
    assert_eq!(
        last_crossing("05:00", now).unwrap().to_rfc3339(),
        "2026-09-19T05:00:00+00:00"
    );
    // The minute itself counts as crossed, so a driver ticking at 03:00:00
    // does not wait a day.
    assert_eq!(
        last_crossing("04:30", now).unwrap().to_rfc3339(),
        "2026-09-20T04:30:00+00:00"
    );
    assert!(last_crossing("4:30", now).is_none());
}

/// A document with something in every shape the write has to preserve:
/// an override that is set, an override cleared to `null`, an override
/// left absent, and a key layer 2 owns outright.
fn seeded(path: &Path) {
    std::fs::write(
        path,
        r#"{
          "schema": "mica/update-config/v1",
          "policy": "check",
          "checkIntervalMinutes": null,
          "source": { "maxBytes": 123456789 },
          "maintenance": { "windows": [ { "days": ["mon"], "start": "02:00", "end": "04:00" } ] }
        }"#,
    )
    .expect("seed the document");
}

#[test]
fn a_saved_document_loads_back_with_absent_and_null_still_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    seeded(&path);

    let before = load_updates(&path).expect("the seed loads");
    save_updates(&path, &before).expect("it saves");
    let after = load_updates(&path).expect("it loads back");

    // `policy` was set, `checkIntervalMinutes` was an explicit `null` and
    // `source.url` was never named. A save that flattened the last two
    // into each other would rewrite what the operator said.
    assert_eq!(after.policy, Some(Some(UpdateMode::Check)));
    assert_eq!(after.check_interval_minutes, Some(None));
    assert_eq!(after.source.url, None);
    assert_eq!(after.source.max_bytes, 123_456_789);
    assert_eq!(after.maintenance.windows.len(), 1);

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["checkIntervalMinutes"], Value::Null);
    assert!(
        !written.as_object().unwrap().contains_key("policy") || written["policy"] == json!("check")
    );
    assert!(
        written["source"].as_object().unwrap().get("url").is_none(),
        "a key the operator never wrote must not appear: {written}"
    );
}

#[test]
fn a_saved_document_names_its_schema_even_when_the_one_it_replaced_did_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();

    write_updates(&path, r#"{ "policy": "check" }"#).expect("the write lands");

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["schema"], json!(UPDATES_SCHEMA_TAG));
}

#[test]
fn a_saved_document_is_mode_0600_and_leaves_no_temporary_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    write_updates(&path, r#"{ "policy": "off" }"#).expect("the write lands");

    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "the namespace's documents are 0600"
    );
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        vec![std::ffi::OsString::from("updates.json")],
        "the rename consumed the temporary sibling"
    );
}

/// The operator document refuses an unknown key, such as `channel`, and
/// another schema tag.
#[test]
fn an_operator_document_refuses_an_unknown_key_or_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    for refused in [
        r#"{ "schema": "mica/update-config/v1", "source": { "channel": "beta" } }"#,
        r#"{ "schema": "mica/update-config/v2" }"#,
    ] {
        std::fs::write(&path, refused).unwrap();
        assert!(load_updates(&path).is_err(), "{refused}");
    }
}

#[test]
fn a_patch_changes_the_keys_it_names_and_leaves_every_other_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    seeded(&path);

    let saved = write_updates(&path, r#"{ "checkAt": "03:00" }"#).expect("the write lands");

    assert_eq!(saved.check_at, Some(Some("03:00".to_string())));
    // Everything the patch did not name survived, including the keys no
    // console renders.
    assert_eq!(saved.policy, Some(Some(UpdateMode::Check)));
    assert_eq!(saved.check_interval_minutes, Some(None));
    assert_eq!(saved.source.max_bytes, 123_456_789);
    assert_eq!(saved.maintenance.windows.len(), 1);
}

#[test]
fn an_explicit_null_clears_an_override_and_an_absent_key_leaves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    seeded(&path);

    let saved = write_updates(&path, r#"{ "source": { "url": null } }"#).expect("the write lands");

    // `null` is the operator saying "take the baked address again", and
    // it is recorded as `null` rather than as absence.
    assert_eq!(saved.source.url, Some(None));
    assert_eq!(saved.source.max_bytes, 123_456_789);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["source"]["url"], Value::Null);
}

#[test]
fn a_patch_may_re_point_the_device_at_another_address() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    seeded(&path);

    write_updates(
        &path,
        r#"{ "source": { "url": "https://updates.example/repo" } }"#,
    )
    .expect("the write lands");

    let baked = BakedUpdate {
        source: Some("https://baked.example/repo".to_string()),
        ..BakedUpdate::code_defaults()
    };
    let effective = resolve(&baked, load_updates(&path).unwrap());
    assert_eq!(
        effective.selection.unwrap().url.as_deref(),
        Some("https://updates.example/repo"),
        "the override wins over the baked default"
    );
}

#[test]
fn auto_with_no_window_is_refused_at_the_write_and_the_document_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let err = write_updates(&path, r#"{ "policy": "auto" }"#)
        .expect_err("`auto` with no window is not a document this device may hold");

    assert!(
        matches!(err, WriteRefusal::Rejected(_)),
        "the operator's input is what was wrong: {err}"
    );
    assert!(
        err.to_string().contains(AUTO_NEEDS_A_WINDOW),
        "the refusal states the rule: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a refused write replaces nothing"
    );
}

#[test]
fn auto_with_a_window_in_the_same_patch_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();

    let saved = write_updates(
        &path,
        r#"{ "policy": "auto",
             "maintenance": { "windows": [ { "start": "02:00", "end": "04:00" } ] } }"#,
    )
    .expect("the rule is about the resulting document, not about the order of two writes");

    assert_eq!(saved.policy, Some(Some(UpdateMode::Auto)));
}

#[test]
fn a_patch_naming_a_trust_anchor_is_refused_by_that_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    // At the top level, nested inside an object the schema does have, and
    // inside an array: the scan must have no hole, because the write
    // route is the surface where somebody would try.
    let patches = [
        r#"{ "trust": { "signingKeys": ["k"] } }"#,
        r#"{ "signingKeys": ["k"] }"#,
        r#"{ "signingKeyId": "abc" }"#,
        r#"{ "signingKeyIds": ["abc"] }"#,
        r#"{ "source": { "rootPath": "/tmp/root.json" } }"#,
        r#"{ "source": { "keyring": "/tmp/keys" } }"#,
        r#"{ "maintenance": { "windows": [ { "start": "02:00", "end": "04:00", "keyring": "x" } ] } }"#,
    ];
    for patch in patches {
        let err = write_updates(&path, patch).expect_err("an anchor is not the operator's");
        let WriteRefusal::Rejected(ConfigError::Anchor { key, .. }) = &err else {
            panic!("{patch} must be refused as an anchor, was {err}");
        };
        assert!(
            patch.contains(key.as_str()),
            "the refusal names the key it found: {key} not in {patch}"
        );
        assert!(
            err.to_string().contains(key),
            "the message carries the key: {err}"
        );
        assert!(!path.exists(), "a refused write creates nothing");
    }
}

#[test]
fn a_patch_naming_a_key_this_document_does_not_have_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    seeded(&path);
    let before = std::fs::read_to_string(&path).unwrap();

    for (patch, key) in [
        (r#"{ "source": { "repoDir": "/tmp/mirror" } }"#, "repoDir"),
        (r#"{ "source": { "maxBytes": 1 } }"#, "maxBytes"),
        (r#"{ "source": { "channel": "beta" } }"#, "channel"),
        (r#"{ "autoCheck": { "intervalMinutes": 60 } }"#, "autoCheck"),
        (r#"{ "schema": "mica/update-config/v1" }"#, "schema"),
    ] {
        let err = write_updates(&path, patch).expect_err("{patch} is not a key of the patch");
        assert!(
            err.to_string().contains(key),
            "the refusal names the offending field: {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}

#[test]
fn a_document_that_does_not_load_is_not_patched_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    std::fs::write(&path, "{ this is not json").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let err =
        write_updates(&path, r#"{ "policy": "off" }"#).expect_err("there is no base to merge over");

    assert!(
        matches!(err, WriteRefusal::Unreadable(_)),
        "the file is what is wrong, not the request: {err}"
    );
    assert!(err.to_string().contains("updates.json"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a blind overwrite would drop keys the operator cannot see"
    );
}

#[test]
fn a_write_into_a_missing_namespace_refuses_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // `/mica/config/` absent is the DATA medium not mounted, so a
    // device that created it would write the operator's policy onto the
    // root filesystem where the next boot would not look.
    let path = dir.path().join("not-mounted").join("updates.json");

    let err = write_updates(&path, r#"{ "policy": "off" }"#).expect_err("nowhere to write");

    assert!(matches!(err, WriteRefusal::Unwritable(_)), "{err}");
    assert!(!dir.path().join("not-mounted").exists());
}

#[test]
fn a_patch_that_is_not_json_is_the_requests_fault() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.json");
    let err = write_updates(&path, "not json").expect_err("not a patch");
    assert!(matches!(err, WriteRefusal::Rejected(_)), "{err}");
}
