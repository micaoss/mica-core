//! What a snapshot carries after redaction.

use crate::redact;
use serde_json::json;

use super::*;

/// The negative fixture: every planted secret is absent from the
/// produced bytes, whichever way it was planted.
#[test]
pub(super) fn every_planted_secret_is_absent_from_the_produced_snapshot() {
    let (redacted, stats) = redact_snapshot(fixture());
    let text = redacted.to_string();
    for marker in [
        "hunter2-marker",
        "cafebabe-marker",
        "pemmarker",
        "should-be-redacted-marker",
        "HomeNet-marker",
        "aa:bb:cc:dd:ee:ff",
        "aa:bb:cc:dd:ee:01",
        "02:42:ac:11:00:02",
        "wifi-psk-marker",
        "hash-marker",
        "tokenhash-marker",
        "wg-private-marker",
        "bearer-marker",
        "registry-marker",
        "wifiPassphraseCache",
        "registryAuth",
    ] {
        assert!(!text.contains(marker), "{marker} shipped:\n{text}");
    }
    assert!(stats.dropped_fields > 0);
    assert!(stats.redacted_fields > 0);
    // The identifying network fields are present as the sentinel, so a
    // reader can tell "redacted" from "the device had none".
    assert_eq!(
        redacted["network"]["interfaces"]["entries"][0]["hardwareAddress"],
        redact::REDACTED
    );
    assert_eq!(
        redacted["network"]["interfaces"]["entries"][1]["wifi"]["ssid"],
        redact::REDACTED
    );
    assert_eq!(
        redacted["network"]["wifi"]["associations"][0]["bssid"],
        redact::REDACTED
    );
    // The journal line carrying a PSK is gone whole; the one carrying a
    // MAC keeps its message with the MAC replaced.
    assert_eq!(redacted["journal"]["lines"][1], REDACTED_LINE);
    assert_eq!(redacted["journal"]["lines"][3], REDACTED_LINE);
    assert_eq!(
        redacted["journal"]["lines"][2],
        "2026-09-02T00:00:02+0000 networkd[7]: eth0: link <mac> up"
    );
    // A dynamic key that is itself a secret name is dropped with its
    // value; the benign neighbour stays.
    assert!(redacted["failures"]["health"].get("psk").is_none());
    assert_eq!(redacted["failures"]["health"]["var"]["status"], "degraded");
}

/// The positive fixture: the benign evidence the contract keeps is all
/// present after the pass, so the allowlist is not passing by dropping
/// everything.
#[test]
pub(super) fn every_benign_member_survives_the_pass() {
    let (redacted, _) = redact_snapshot(fixture());
    for (pointer, expected) in [
        ("/schemaVersion", json!(1)),
        ("/collectedAt", json!("2026-09-02T00:00:00Z")),
        ("/release/board/model", json!("Vendor CX3576")),
        ("/release/kernel/release", json!("6.1.115-mica")),
        (
            "/system/machineId/id",
            json!("0123456789abcdef0123456789abcdef"),
        ),
        ("/system/system/fileEpoch/epoch", json!(1_577_836_800)),
        ("/system/packages/entries/0/version", json!("0.1.1-1")),
        ("/system/deployment/id", json!("a".repeat(64))),
        ("/boot/reset/reason", json!("watchdog")),
        (
            "/boot/reset/evidence/watchdogBootstatus/0/flags/0",
            json!("cardReset"),
        ),
        ("/boot/update/boot/contentVerified", json!(true)),
        ("/boot/update/install/deploymentId", json!("b".repeat(64))),
        (
            "/journal/lines/0",
            json!("2026-09-02T00:00:00+0000 systemd[1]: Failed to start x.service."),
        ),
        ("/journal/bounds/maxLines", json!(400)),
        ("/failures/units/entries/0/name", json!("x.service")),
        ("/failures/tasks/0/outcome", json!("failed")),
        ("/failures/tasks/0/message", json!("render failed")),
        ("/failures/health/var/status", json!("degraded")),
        ("/failures/health/var/detail", json!("/var at 91%")),
        ("/storage/tiers/0/space/usedPercent", json!(85)),
        (
            "/storage/tiers/0/check/unit",
            json!("systemd-fsck@dev-mmcblk0p11.service"),
        ),
        ("/storage/namespaces/binds/0/probe/passed", json!(true)),
        (
            "/storage/media/0/health/lifetimeEstimates/0/usedPercentMax",
            json!(10),
        ),
        ("/storage/policy/watchedTiers/0", json!("data")),
        ("/storage/lifecycle/encryption", json!("unsupported")),
        ("/time/status", json!("synchronized")),
        ("/time/sample/correction", json!("slew")),
        ("/telemetry/thermal/zones/0/milliCelsius", json!(48250)),
        ("/telemetry/watchdog/devices/0/bootstatus/raw", json!(32)),
        ("/network/interfaces/entries/0/link/carrier", json!(true)),
        (
            "/network/interfaces/entries/0/addresses/0/address",
            json!("192.0.2.10"),
        ),
        (
            "/network/interfaces/entries/0/dhcp/lease/server",
            json!("192.0.2.1"),
        ),
        ("/network/interfaces/entries/0/dns/0", json!("192.0.2.1")),
        ("/network/interfaces/entries/1/wifi/associated", json!(true)),
        ("/network/interfaces/entries/1/wifi/rssiDbm", json!(-51)),
        (
            "/network/defaultRoutes/entries/0/gateway",
            json!("192.0.2.1"),
        ),
        ("/network/dns/probe/result", json!("resolved")),
        ("/network/dns/probe/detail", json!("4 address(es)")),
        ("/network/capabilities/cellular/supported", json!(false)),
    ] {
        assert_eq!(
            redacted.pointer(pointer),
            Some(&expected),
            "{pointer} did not survive: {}",
            redacted
        );
    }
}

/// The fail-closed rule stated as a rule: an object under a scalar rule,
/// a member of the wrong shape, and a whole unnamed section all go.
#[test]
pub(super) fn what_the_schema_does_not_name_does_not_ship() {
    let (redacted, stats) = redact_snapshot(json!({
        "schemaVersion": { "nested": "object under a scalar rule" },
        "time": { "status": "synchronized", "server": "a string under an object rule" },
        "unnamedSection": { "anything": 1 },
        "journal": { "lines": "not an array" },
    }));
    assert_eq!(
        redacted,
        json!({ "time": { "status": "synchronized" }, "journal": { } })
    );
    assert_eq!(stats.dropped_fields, 4);
}
