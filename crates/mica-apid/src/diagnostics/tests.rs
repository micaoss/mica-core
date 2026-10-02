use super::*;
use serde_json::{Value, json};

mod collect;
mod redaction;
mod store;

#[test]
fn a_secret_marker_replaces_the_whole_string() {
    for text in [
        "wpa_supplicant[420]: psk=deadbeef",
        "Set PASSWORD for root",
        "Authorization: Bearer abc.def",
        "-----BEGIN PRIVATE KEY-----",
        "apid: token minted",
        "private_key=/mnt/state/wg0.key",
    ] {
        assert_eq!(scrub(text), (REDACTED_LINE.to_string(), 1), "{text}");
    }
}

#[test]
fn a_hardware_address_is_replaced_and_an_ip_is_not() {
    let (text, replaced) = scrub(
        "eth0: link 02:42:ac:11:00:02 (02:42:AC:11:00:02), gw 192.0.2.1 fe80::42:acff:fe11:2",
    );
    assert_eq!(
        text,
        "eth0: link <mac> (<mac>), gw 192.0.2.1 fe80::42:acff:fe11:2"
    );
    assert_eq!(replaced, 2);
    assert_eq!(scrub("plain text").0, "plain text");
    assert_eq!(
        scrub("2026-09-02T00:00:00+00:00 ok").0,
        "2026-09-02T00:00:00+00:00 ok"
    );
}

#[test]
fn a_long_string_is_cut_on_a_character_boundary() {
    let long = "€".repeat(MAX_TEXT_BYTES);
    let (text, _) = scrub(&long);
    assert!(text.len() <= MAX_TEXT_BYTES + "…[cut]".len());
    assert!(text.ends_with("…[cut]"));
}

/// A snapshot carrying every documented benign member, with a secret
/// planted at every kind of place a secret could land.
fn fixture() -> Value {
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "collectedAt": "2026-09-02T00:00:00Z",
        "release": {
            "board": { "available": true, "model": "Vendor CX3576", "source": "devicetree" },
            "release": { "available": true, "id": "debian", "prettyName": "Debian 13" },
            "kernel": { "available": true, "release": "6.1.115-mica", "version": "#1 SMP" },
        },
        "system": {
            "machineId": { "available": true, "id": "0123456789abcdef0123456789abcdef" },
            "board": { "available": true, "model": "Vendor CX3576", "source": "devicetree" },
            "kernel": { "available": true, "release": "6.1.115-mica", "version": "#1 SMP" },
            "release": { "available": true, "id": "debian" },
            "system": {
                "available": true, "version": "0.1.1-1", "package": "micad",
                "fileEpoch": { "available": true, "epoch": 1577836800, "date": "2020-01-01T00:00:00Z" },
            },
            "daemon": { "name": "micad", "version": "0.1.1-1" },
            "packages": { "available": true, "count": 2, "micaCount": 1, "malformedRows": 0, "truncated": false,
                "entries": [{ "name": "micad", "version": "0.1.1-1", "architecture": "arm64", "mica": true },
                            { "name": "systemd", "version": "257.7-1", "architecture": "arm64", "mica": false }] },
            "deployment": { "available": true, "id": "a".repeat(64), "confirmed": true },
            "uptime": { "available": true, "seconds": 4242 },
            // Unclassified: a member no schema names must not ship.
            "wifiPassphraseCache": "hunter2-marker",
        },
        "boot": {
            "deployment": { "available": true, "id": "a".repeat(64) },
            "uptime": { "available": true, "seconds": 4242 },
            "reset": { "available": true, "reason": "watchdog", "detail": "d",
                "evidence": { "watchdogBootstatus": [{ "device": "watchdog0", "flags": ["cardReset"] }], "pstore": { "available": false, "detail": "no pstore" } } },
            "update": { "boot": {"deploymentId":"a".repeat(64),"contentVerified":true},
                "state":{"current":"a".repeat(64),"highestGeneration":7,"failed":[]},
                "install": {"status":"done","deploymentId":"b".repeat(64),"requested_by":":1.7"},
                "last_action":{"action":"confirm","deploymentId":"a".repeat(64),"requested_by":":1.7"} },
        },
        "journal": { "available": true, "scope": "current boot", "priority": "warning", "lineCount": 4, "sourceLines": 4, "sourceBytes": 100, "truncated": false,
            "bounds": { "maxLines": 400, "maxBytes": 131072, "maxLineBytes": 1024 },
            "lines": [
                "2026-09-02T00:00:00+0000 systemd[1]: Failed to start x.service.",
                "2026-09-02T00:00:01+0000 wpa_supplicant[42]: wlan0: psk=cafebabe-marker",
                "2026-09-02T00:00:02+0000 networkd[7]: eth0: link 02:42:ac:11:00:02 up",
                "2026-09-02T00:00:03+0000 sshd[9]: -----BEGIN OPENSSH PRIVATE KEY----- pemmarker",
            ] },
        "failures": {
            "units": { "available": true, "count": 1, "truncated": false,
                "entries": [{ "name": "x.service", "description": "X", "loadState": "loaded", "activeState": "failed", "subState": "failed" }] },
            "tasks": [{ "id": "t-1", "operation": "settings-write", "dotPath": "network", "source": "api", "status": "finished",
                        "enqueuedAt": "2026-09-02T00:00:00Z", "outcome": "failed", "message": "render failed", "foldedCount": 0 }],
            "health": { "var": { "status": "degraded", "detail": "/var at 91%" },
                        "psk": { "status": "should-be-redacted-marker", "detail": "x" } },
        },
        "storage": {
            "tiers": [{ "name": "data", "role": "ext4", "partitionLabel": "data", "present": true, "device": "/dev/mmcblk0p11", "mounted": true, "mount": "/mnt/data", "filesystem": "ext4", "readOnly": false,
                        "space": { "totalBytes": 1000, "usedBytes": 850, "freeBytes": 100, "reservedBytes": 50, "usedPercent": 85 }, "pressure": "warning",
                        "check": { "recorded": true, "unit": "systemd-fsck@dev-mmcblk0p11.service", "activeState": "inactive", "result": "success", "exitStatus": 0 } }],
            "namespaces": { "sharedCapacityTier": "data", "detail": "one pool",
                "binds": [{ "name": "mica", "mount": "/mica", "source": "/mnt/data/mica", "owner": "system", "readiness": "ready", "mounted": true, "device": "/dev/mmcblk0p11", "readOnly": false, "sourceIsDirectory": true, "probe": { "attempted": true, "passed": true } }] },
            "media": [{ "name": "mmcblk0", "kind": "emmc", "sizeBytes": 32000000000u64, "model": "DG4032", "rotational": false,
                        "health": { "supported": true, "source": "sysfs", "raw": { "lifeTime": "0x01 0x01", "preEolInfo": "0x01" }, "lifetimeEstimates": [{ "raw": "0x01", "usedPercentMin": 0, "usedPercentMax": 10 }], "preEol": "normal" } }],
            "policy": { "warningPercent": 80, "warningClearPercent": 75, "criticalPercent": 90, "criticalClearPercent": 85, "watchedTiers": ["data"] },
            "lifecycle": { "encryption": "unsupported", "factoryReset": "unsupported" },
        },
        "time": { "status": "synchronized", "synchronized": true, "server": { "name": "0.pool.ntp.org", "address": "192.0.2.7" },
                  "sample": { "leap": 0, "stratum": 2, "spike": false, "offsetSeconds": 0.012, "packetCount": 7, "correction": "slew" } },
        "telemetry": {
            "thermal": { "available": true, "zones": [{ "sensor": "thermal_zone0", "label": "soc-thermal", "milliCelsius": 48250 }], "hwmon": [] },
            "watchdog": { "available": true, "devices": [{ "device": "watchdog0", "identity": "dw_wdt", "state": "active", "timeoutSeconds": 30, "bootstatus": { "available": true, "raw": 32, "flags": ["cardReset"] }, "nowayout": true }] },
        },
        "network": {
            "interfaces": { "available": true, "count": 2, "entries": [
                { "name": "eth0", "index": 2, "type": "ether", "driver": "stmmac", "mtu": 1500,
                  "link": { "administrativeState": "configured", "operationalState": "routable", "carrierState": "carrier", "carrier": true, "onlineState": "online", "addressState": "routable" },
                  "hardwareAddress": "02:42:ac:11:00:02",
                  "addresses": [{ "family": "ipv4", "address": "192.0.2.10", "prefixLength": 24, "scope": "global", "configSource": "DHCPv4" }],
                  "dhcp": { "available": true, "inferred": false, "state": "bound", "lease": { "address": "192.0.2.10", "prefixLength": 24, "server": "192.0.2.1", "router": "192.0.2.1", "lifetimeSeconds": 86400 } },
                  "dns": ["192.0.2.1"] },
                { "name": "wlan0", "index": 3, "type": "wlan", "link": { "carrierState": "carrier", "carrier": true },
                  "hardwareAddress": "aa:bb:cc:dd:ee:01", "addresses": [], "dhcp": { "available": false, "detail": "none" }, "dns": [],
                  "wifi": { "interface": "wlan0", "available": true, "state": "COMPLETED", "associated": true, "ssid": "HomeNet-marker", "bssid": "aa:bb:cc:dd:ee:ff", "frequencyMhz": 5180, "keyManagement": "WPA2-PSK", "rssiDbm": -51, "linkSpeedMbps": 433,
                            "psk": "wifi-psk-marker" } },
            ] },
            "defaultRoutes": { "available": true, "count": 1, "entries": [{ "family": "ipv4", "gateway": "192.0.2.1", "interface": "eth0", "interfaceIndex": 2, "metric": 1024, "protocol": "dhcp", "table": "main", "configSource": "DHCPv4" }] },
            "dns": { "available": true, "linkServers": ["192.0.2.1"], "resolverServers": ["192.0.2.1"], "probe": { "available": true, "name": "0.debian.pool.ntp.org", "reachable": true, "result": "resolved", "detail": "4 address(es)" } },
            "wifi": { "available": true, "associations": [{ "interface": "wlan0", "available": true, "state": "COMPLETED", "associated": true, "ssid": "HomeNet-marker", "bssid": "aa:bb:cc:dd:ee:ff" }] },
            "capabilities": { "wifi": { "supported": true, "interfaces": ["wlan0"], "detail": "d" }, "bluetooth": { "supported": false, "adapters": [], "detail": "d" }, "cellular": { "supported": false, "interfaces": [], "detail": "d" } },
        },
        // Secrets under the names the live routes already deny, and
        // under names the snapshot denies, at the top level and nested.
        "access": { "webAdmin": { "password_hash": "hash-marker" }, "apiTokens": [{ "hash": "tokenhash-marker" }] },
        "privateKey": "wg-private-marker",
        "token": "bearer-marker",
        "registryAuth": { "auth": "registry-marker" },
    })
}
