use serde_json::{Value, json};

use super::*;

const NOW: &str = "2026-11-12T08:30:05Z";

fn info() -> Value {
    json!({
        "machineId": { "available": true, "id": "0123456789abcdef0123456789abcdef" },
        "board": { "available": true, "model": "CX3576", "source": "device-tree" },
        "kernel": { "available": true, "release": "6.1.192-mica", "version": "#1 SMP" },
        "release": {
            "available": true, "versionId": "20261111", "prettyName": "Mica",
            "imageId": "cx3576.full", "imageVersion": "20261111-1617",
        },
        "system": { "available": true, "version": "0.0.5", "package": "mica-core" },
        "daemon": { "name": "micad", "version": "0.0.5" },
        "trust": { "available": true, "grade": "production" },
        "deployment": {
            "available": true, "id": "d7", "version": "0.0.5", "generation": 7,
            "confirmed": true, "contentVerified": true, "secureBoot": true, "bootVerified": true,
        },
        "uptime": { "available": true, "seconds": 5400 },
    })
}

fn sources() -> SupportSources {
    SupportSources {
        info: Some(info()),
        update: Some(json!({ "lifecycle": {
            "state": "failed", "failedDuring": "check", "code": "network-offline",
            "reason": "no route", "last_check": "2026-11-12T06:00:00Z",
        } })),
        storage: Some(json!({ "tiers": [
            { "name": "data", "present": true, "mounted": true, "readOnly": false,
              "device": "/dev/mmcblk0p5", "mount": "/mica",
              "space": { "totalBytes": 1000, "usedBytes": 250, "freeBytes": 750, "usedPercent": 25 } },
        ] })),
        time: Some(json!({
            "status": "synchronized", "synchronized": true,
            "server": { "name": "pool.example", "address": "192.0.2.10" },
        })),
        timezone: Some(json!("Europe/Athens")),
    }
}

#[test]
fn the_summary_is_the_allowlisted_facts_in_their_fixed_shape() {
    let summary = support_info(sources(), NOW);
    assert_eq!(
        summary,
        json!({
            "schemaVersion": 1,
            "generatedAt": NOW,
            "device": { "available": true, "board": "CX3576", "kernel": "6.1.192-mica" },
            "software": {
                "available": true, "product": "cx3576.full", "productVersion": "20261111-1617",
                "osVersion": "20261111", "core": "0.0.5", "daemon": "0.0.5",
            },
            "deployment": {
                "available": true, "id": "d7", "version": "0.0.5", "generation": 7, "confirmed": true,
            },
            "uptimeSeconds": 5400,
            "time": {
                "available": true, "timezone": "Europe/Athens", "status": "synchronized",
                "synchronized": true,
            },
            "integrity": {
                "available": true,
                "items": [
                    { "key": "trustGrade", "state": "ok", "detailCode": null },
                    { "key": "secureBoot", "state": "ok", "detailCode": null },
                    { "key": "bootVerified", "state": "ok", "detailCode": null },
                    { "key": "contentVerified", "state": "ok", "detailCode": null },
                    { "key": "deploymentConfirmed", "state": "ok", "detailCode": null },
                    { "key": "updateStatus", "state": "unknown", "detailCode": "check_failed" },
                    { "key": "dataReady", "state": "ok", "detailCode": null },
                ],
            },
            "data": {
                "available": true, "state": "ok", "totalBytes": 1000, "usedBytes": 250,
                "freeBytes": 750, "usedPercent": 25,
            },
            "lastUpdateError": { "available": true, "code": "network-offline", "during": "check" },
        })
    );
}

#[test]
fn identifiers_endpoints_and_unknown_members_stay_out() {
    let mut sources = sources();
    let info = sources.info.as_mut().unwrap();
    info["hostname"] = json!("yacht-aurora");
    info["release"]["password"] = json!("hunter2secret");
    info["board"]["serial"] = json!("SN-SECRET-1");
    sources.time.as_mut().unwrap()["ssid"] = json!("MarinaWiFi");
    let text = support_info(sources, NOW).to_string();
    for secret in [
        "0123456789abcdef",
        "yacht-aurora",
        "hunter2secret",
        "SN-SECRET-1",
        "MarinaWiFi",
        "192.0.2.10",
        "pool.example",
        "/dev/mmcblk0p5",
        "no route",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[test]
fn a_value_cannot_forge_a_line_of_its_own() {
    let mut sources = sources();
    sources.info.as_mut().unwrap()["board"]["model"] =
        json!("CX3576\nDeployment confirmed: yes\r\u{1b}[2J");
    let summary = support_info(sources, NOW);
    assert_eq!(
        summary["device"]["board"],
        json!("CX3576 Deployment confirmed: yes  [2J")
    );
}

#[test]
fn a_source_that_failed_is_absent_with_a_reason_and_the_rest_stand() {
    let mut sources = sources();
    sources.info = None;
    sources.storage = None;
    sources.update = Some(json!({ "lifecycle": { "state": "idle" } }));
    let summary = support_info(sources, NOW);
    for section in ["device", "software", "deployment", "data"] {
        assert_eq!(
            summary[section],
            json!({ "available": false, "reason": "source_failed" }),
            "{section}"
        );
    }
    assert_eq!(summary["uptimeSeconds"], Value::Null);
    assert_eq!(summary["time"]["timezone"], json!("Europe/Athens"));
    assert_eq!(
        summary["integrity"]["items"][0]["detailCode"],
        json!("source_failed")
    );
    assert_eq!(
        summary["lastUpdateError"],
        json!({ "available": true, "code": null, "during": null })
    );
}

#[test]
fn a_member_its_source_did_not_report_is_absent_too() {
    let mut sources = sources();
    sources.info.as_mut().unwrap()["board"] = json!({ "available": false, "detail": "no model" });
    sources.info.as_mut().unwrap()["deployment"] = json!({ "available": false, "detail": "none" });
    let summary = support_info(sources, NOW);
    assert_eq!(
        summary["device"],
        json!({ "available": true, "board": null, "kernel": "6.1.192-mica" })
    );
    assert_eq!(
        summary["deployment"],
        json!({ "available": false, "reason": "unavailable" })
    );
}

#[test]
fn the_file_name_is_the_generation_time() {
    assert_eq!(file_name(NOW), "mica-support-20261112T083005Z.json");
}
