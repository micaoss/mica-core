use serde_json::{Value, json};

use super::*;

const NOW: &str = "2026-11-11T12:00:00Z";

fn info() -> Value {
    json!({
        "trust": { "available": true, "grade": "production" },
        "deployment": {
            "available": true, "id": "d1", "version": "0.0.6", "generation": 7,
            "confirmed": true, "contentVerified": true, "secureBoot": true, "bootVerified": true,
        },
    })
}

fn update() -> Value {
    json!({ "lifecycle": { "state": "idle", "last_check": "2026-11-11T06:00:00Z" } })
}

fn storage() -> Value {
    json!({ "tiers": [
        { "name": "system", "present": true, "mounted": true, "readOnly": true },
        { "name": "data", "present": true, "mounted": true, "readOnly": false },
    ] })
}

fn sources() -> Sources {
    Sources {
        info: Some(info()),
        update: Some(update()),
        storage: Some(storage()),
        ssh_keys: Some(2),
        custom_ui: Some(false),
    }
}

/// The item of `key`.
fn item(report: &Value, key: &str) -> Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == json!(key))
        .unwrap_or_else(|| panic!("no item {key} in {report}"))
        .clone()
}

fn state_of(report: &Value, key: &str) -> (String, Value) {
    let item = item(report, key);
    (
        item["state"].as_str().unwrap().to_string(),
        item["detailCode"].clone(),
    )
}

#[test]
fn a_production_confirmed_device_is_ok_throughout() {
    let report = integrity(sources(), NOW);
    assert_eq!(report["checkedAt"], json!(NOW));
    let keys: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "trustGrade",
            "secureBoot",
            "bootVerified",
            "contentVerified",
            "deploymentConfirmed",
            "updateStatus",
            "dataReady",
        ]
    );
    for key in keys {
        assert_eq!(
            state_of(&report, key),
            ("ok".to_string(), Value::Null),
            "{key}"
        );
    }
    assert_eq!(item(&report, "trustGrade")["value"], json!("production"));
    assert_eq!(item(&report, "deploymentConfirmed")["value"], json!(7));
    // The update item carries the check's own time, not the read's.
    assert_eq!(
        item(&report, "updateStatus")["checkedAt"],
        json!("2026-11-11T06:00:00Z")
    );
    assert_eq!(
        report["extensions"],
        json!([
            { "key": "sshAuthorizedKeys", "value": 2 },
            { "key": "customUi", "value": false },
        ])
    );
}

#[test]
fn development_material_and_unverified_boot_need_attention() {
    let mut sources = sources();
    let info = sources.info.as_mut().unwrap();
    info["trust"] = json!({ "available": true, "grade": "development", "developmentDomains": ["root", "core"] });
    info["deployment"]["secureBoot"] = json!(false);
    info["deployment"]["bootVerified"] = json!(false);
    info["deployment"]["contentVerified"] = json!(false);
    info["deployment"]["confirmed"] = json!(false);
    let report = integrity(sources, NOW);
    assert_eq!(
        state_of(&report, "trustGrade"),
        ("attention".into(), json!("development"))
    );
    assert_eq!(
        item(&report, "trustGrade")["domains"],
        json!(["root", "core"])
    );
    assert_eq!(
        state_of(&report, "secureBoot"),
        ("attention".into(), json!("disabled"))
    );
    assert_eq!(
        state_of(&report, "bootVerified"),
        ("attention".into(), json!("not_verified"))
    );
    assert_eq!(
        state_of(&report, "contentVerified"),
        ("attention".into(), json!("not_verified"))
    );
    assert_eq!(
        state_of(&report, "deploymentConfirmed"),
        ("attention".into(), json!("trial_boot"))
    );
}

#[test]
fn a_missing_fact_is_unknown_and_never_invented() {
    let mut sources = sources();
    let info = sources.info.as_mut().unwrap();
    info["trust"] = json!({ "available": false, "detail": "no anchor" });
    info["deployment"] = json!({ "available": false, "detail": "no backend" });
    let report = integrity(sources, NOW);
    for key in [
        "trustGrade",
        "secureBoot",
        "bootVerified",
        "contentVerified",
        "deploymentConfirmed",
    ] {
        assert_eq!(
            state_of(&report, key),
            ("unknown".into(), json!("unavailable")),
            "{key}"
        );
        assert_eq!(item(&report, key)["value"], Value::Null, "{key}");
    }
}

#[test]
fn the_update_item_follows_the_last_check() {
    let with = |lifecycle: Value| {
        let mut sources = sources();
        sources.update = Some(json!({ "lifecycle": lifecycle }));
        integrity(sources, NOW)
    };
    let report = with(
        json!({ "state": "failed", "failedDuring": "check", "last_check": "2026-11-11T06:00:00Z" }),
    );
    assert_eq!(
        state_of(&report, "updateStatus"),
        ("unknown".into(), json!("check_failed"))
    );
    // A failed fetch is not a failed check.
    let report = with(
        json!({ "state": "failed", "failedDuring": "fetch", "last_check": "2026-11-11T06:00:00Z" }),
    );
    assert_eq!(state_of(&report, "updateStatus").0, "ok");
    let report = with(
        json!({ "state": "idle", "last_check": "2026-11-11T06:00:00Z",
        "coreCheckFailed": { "code": "unknown", "reason": "x" } }),
    );
    assert_eq!(
        state_of(&report, "updateStatus"),
        ("unknown".into(), json!("core_check_failed"))
    );
    let report = with(json!({ "state": "idle" }));
    assert_eq!(
        state_of(&report, "updateStatus"),
        ("unknown".into(), json!("never_checked"))
    );
    let report = with(json!({ "state": "idle", "last_check": "2026-11-01T06:00:00Z" }));
    assert_eq!(
        state_of(&report, "updateStatus"),
        ("unknown".into(), json!("stale"))
    );
    let report = with(
        json!({ "state": "idle", "last_check": "2026-11-11T06:00:00Z",
        "available": { "deploymentId": "d2", "version": "0.0.7", "kind": "deployment" } }),
    );
    assert_eq!(
        state_of(&report, "updateStatus"),
        ("attention".into(), json!("update_available"))
    );
    assert_eq!(item(&report, "updateStatus")["value"], json!("0.0.7"));
}

#[test]
fn data_that_is_not_mounted_or_read_only_needs_attention() {
    let with = |data: Value| {
        let mut sources = sources();
        sources.storage = Some(json!({ "tiers": [data] }));
        integrity(sources, NOW)
    };
    let report = with(json!({ "name": "data", "present": true, "mounted": false }));
    assert_eq!(
        state_of(&report, "dataReady"),
        ("attention".into(), json!("not_mounted"))
    );
    let report =
        with(json!({ "name": "data", "present": true, "mounted": true, "readOnly": true }));
    assert_eq!(
        state_of(&report, "dataReady"),
        ("attention".into(), json!("read_only"))
    );
    let report = with(json!({ "name": "data", "present": false }));
    assert_eq!(
        state_of(&report, "dataReady"),
        ("unknown".into(), json!("unavailable"))
    );
}

/// A source that could not be read leaves its items unknown and the others
/// as they are; an extension that could not be read is left out.
#[test]
fn a_failed_source_is_unknown_and_the_rest_stand() {
    let report = integrity(
        Sources {
            info: None,
            update: None,
            ssh_keys: None,
            ..sources()
        },
        NOW,
    );
    for key in [
        "trustGrade",
        "secureBoot",
        "bootVerified",
        "contentVerified",
        "deploymentConfirmed",
        "updateStatus",
    ] {
        assert_eq!(
            state_of(&report, key),
            ("unknown".into(), json!("source_failed")),
            "{key}"
        );
    }
    assert_eq!(state_of(&report, "dataReady").0, "ok");
    assert_eq!(
        report["extensions"],
        json!([{ "key": "customUi", "value": false }])
    );
}
