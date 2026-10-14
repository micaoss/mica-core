//! The redaction schema: every member a snapshot may carry, section by section.

use Rule::Redact as R;
use Rule::Scalar as S;

use super::*;

/// The whole snapshot schema, version [`REDACTION_SCHEMA_VERSION`].
///
/// Every member micad or apid produces is named here or it does not ship.
/// Hardware addresses, SSIDs and BSSIDs are the personally identifying
/// network fields the contract redacts; IP addresses, routes and DNS servers
/// are the troubleshooting evidence it keeps.
pub(super) fn schema() -> Rule {
    let board = || obj(vec![("model", S), ("source", S)]);
    let kernel = || obj(vec![("release", S), ("version", S)]);
    let release = || {
        obj(vec![
            ("name", S),
            ("id", S),
            ("version", S),
            ("versionId", S),
            ("prettyName", S),
            ("buildId", S),
            ("imageId", S),
            ("imageVersion", S),
        ])
    };
    let deployment = || {
        obj(vec![
            ("id", S),
            ("version", S),
            ("generation", S),
            ("kernelId", S),
            ("kernelRelease", S),
            ("rootfsId", S),
            ("confirmed", S),
            ("contentVerified", S),
            ("secureBoot", S),
            ("backend", S),
            ("bootVerified", S),
        ])
    };
    let uptime = || obj(vec![("seconds", S)]);
    let system_member = obj(vec![
        ("version", S),
        ("package", S),
        // `fileEpoch` is the pinned SOURCE_DATE_EPOCH every file in the image
        // carries. A key that is not named here is DROPPED from every snapshot
        // without a word, so renaming a field on that surface without renaming
        // it here would ship snapshots that silently lost it.
        ("fileEpoch", obj(vec![("epoch", S), ("date", S)])),
    ]);
    let packages = obj(vec![
        ("count", S),
        ("micaCount", S),
        ("malformedRows", S),
        ("truncated", S),
        (
            "entries",
            arr(obj(vec![
                ("name", S),
                ("version", S),
                ("architecture", S),
                ("mica", S),
            ])),
        ),
    ]);
    // What the image says about the grade of the material it was signed with,
    // and the domains a development marker names. A support case that opens
    // with "the update was refused" is answered differently depending on this
    // value, so a snapshot that dropped it would send the one fact the reader
    // needed back to the person who has to ask for it again.
    let trust = obj(vec![
        ("grade", S),
        ("developmentDomains", arr(S)),
        ("marker", S),
    ]);
    let system = obj(vec![
        ("machineId", obj(vec![("id", S)])),
        ("board", board()),
        ("kernel", kernel()),
        ("release", release()),
        ("system", system_member),
        ("trust", trust),
        ("daemon", obj(vec![("name", S), ("version", S)])),
        ("packages", packages),
        ("deployment", deployment()),
        ("uptime", uptime()),
    ]);

    let reading = || obj(vec![("sensor", S), ("label", S), ("milliCelsius", S)]);
    let thermal = obj(vec![("zones", arr(reading())), ("hwmon", arr(reading()))]);
    let watchdog = obj(vec![(
        "devices",
        arr(obj(vec![
            ("device", S),
            ("identity", S),
            ("state", S),
            ("timeoutSeconds", S),
            ("timeLeftSeconds", S),
            ("bootstatus", obj(vec![("raw", S), ("flags", arr(S))])),
            ("nowayout", S),
        ])),
    )]);
    let reset = obj(vec![
        ("reason", S),
        (
            "evidence",
            obj(vec![
                (
                    "watchdogBootstatus",
                    arr(obj(vec![("device", S), ("flags", arr(S))])),
                ),
                ("pstore", obj(vec![("records", arr(S))])),
            ]),
        ),
    ]);
    let time = || {
        obj(vec![
            ("status", S),
            ("synchronized", S),
            ("server", obj(vec![("name", S), ("address", S)])),
            (
                "sample",
                obj(vec![
                    ("leap", S),
                    ("stratum", S),
                    ("spike", S),
                    ("offsetSeconds", S),
                    ("packetCount", S),
                    ("correction", S),
                ]),
            ),
        ])
    };

    let update = obj(vec![
        (
            "boot",
            obj(vec![
                ("deploymentId", S),
                ("entry", S),
                ("kernelId", S),
                ("rootfsId", S),
                ("contentVerified", S),
                ("secureBoot", S),
                ("backend", S),
                ("bootVerified", S),
            ]),
        ),
        (
            "state",
            obj(vec![
                ("highestGeneration", S),
                ("current", S),
                ("fallback", S),
                ("candidate", S),
                ("failed", arr(S)),
            ]),
        ),
        (
            "deployments",
            arr(obj(vec![
                ("id", S),
                ("file", S),
                ("generation", S),
                ("triesLeft", S),
                ("version", S),
                ("kernelId", S),
                ("kernelRelease", S),
                ("rootfsId", S),
            ])),
        ),
        (
            "rollback",
            obj(vec![("permitted", S), ("target", S), ("reason", S)]),
        ),
        (
            "install",
            obj(vec![
                ("status", S),
                ("deploymentId", S),
                ("requested_by", S),
                ("error", S),
                ("error_code", S),
            ]),
        ),
        (
            "last_action",
            obj(vec![
                ("action", S),
                ("deploymentId", S),
                ("requested_by", S),
            ]),
        ),
        // Policy source URLs may contain credentials; admit only these facts.
        (
            "lifecycle",
            obj(vec![
                ("state", S),
                ("deploymentId", S),
                ("available", obj(vec![("deploymentId", S), ("version", S)])),
            ]),
        ),
        ("error", S),
    ]);
    let boot = obj(vec![
        ("deployment", deployment()),
        ("uptime", uptime()),
        ("reset", reset),
        ("update", update),
    ]);

    let journal = obj(vec![
        ("scope", S),
        ("priority", S),
        ("lineCount", S),
        ("sourceLines", S),
        ("sourceBytes", S),
        ("truncated", S),
        (
            "bounds",
            obj(vec![("maxLines", S), ("maxBytes", S), ("maxLineBytes", S)]),
        ),
        ("lines", arr(S)),
    ]);
    let task = obj(vec![
        ("id", S),
        ("operation", S),
        ("dotPath", S),
        ("source", S),
        ("status", S),
        ("enqueuedAt", S),
        ("startedAt", S),
        ("finishedAt", S),
        ("outcome", S),
        ("message", S),
        ("foldedCount", S),
    ]);
    let failures = obj(vec![
        (
            "units",
            obj(vec![
                ("count", S),
                ("truncated", S),
                (
                    "entries",
                    arr(obj(vec![
                        ("name", S),
                        ("description", S),
                        ("loadState", S),
                        ("activeState", S),
                        ("subState", S),
                    ])),
                ),
            ]),
        ),
        ("tasks", arr(task)),
        ("health", map(obj(vec![("status", S)]))),
    ]);

    let space = obj(vec![
        ("totalBytes", S),
        ("usedBytes", S),
        ("freeBytes", S),
        ("reservedBytes", S),
        ("usedPercent", S),
    ]);
    let tier = obj(vec![
        ("name", S),
        ("role", S),
        ("partitionLabel", S),
        ("expectedMount", S),
        ("present", S),
        ("device", S),
        ("partitionBytes", S),
        ("mounted", S),
        ("mount", S),
        ("filesystem", S),
        ("readOnly", S),
        ("space", space.clone()),
        ("pressure", S),
        (
            "check",
            obj(vec![
                ("recorded", S),
                ("unit", S),
                ("activeState", S),
                ("result", S),
                ("exitStatus", S),
            ]),
        ),
    ]);
    let bind = obj(vec![
        ("name", S),
        ("mount", S),
        ("source", S),
        ("owner", S),
        ("readiness", S),
        ("mounted", S),
        ("device", S),
        ("readOnly", S),
        ("filesystem", S),
        ("sourceOnData", S),
        ("sourceIsDirectory", S),
        ("space", space),
        ("pressure", S),
        (
            "probe",
            obj(vec![
                ("attempted", S),
                ("passed", S),
                ("error", S),
                ("reason", S),
            ]),
        ),
    ]);
    let health = obj(vec![
        ("supported", S),
        ("reason", S),
        ("source", S),
        ("raw", obj(vec![("lifeTime", S), ("preEolInfo", S)])),
        (
            "lifetimeEstimates",
            arr(obj(vec![
                ("raw", S),
                ("usedPercentMin", S),
                ("usedPercentMax", S),
            ])),
        ),
        ("preEol", S),
    ]);
    let storage = obj(vec![
        ("tiers", arr(tier)),
        (
            "namespaces",
            obj(vec![
                ("sharedCapacityTier", S),
                ("binds", arr(bind)),
                (
                    "directories",
                    arr(obj(vec![("name", S), ("usedBytes", S), ("project", S)])),
                ),
                (
                    "projectQuotas",
                    obj(vec![
                        (
                            "100",
                            obj(vec![
                                ("usedBytes", S),
                                ("limitBytes", S),
                                ("usedInodes", S),
                                ("limitInodes", S),
                            ]),
                        ),
                        (
                            "101",
                            obj(vec![
                                ("usedBytes", S),
                                ("limitBytes", S),
                                ("usedInodes", S),
                                ("limitInodes", S),
                            ]),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "media",
            arr(obj(vec![
                ("name", S),
                ("kind", S),
                ("sizeBytes", S),
                ("model", S),
                ("rotational", S),
                ("health", health),
            ])),
        ),
        (
            "policy",
            obj(vec![
                ("warningPercent", S),
                ("warningClearPercent", S),
                ("criticalPercent", S),
                ("criticalClearPercent", S),
                ("watchedTiers", arr(S)),
            ]),
        ),
        ("lifecycle", map(S)),
    ]);

    let address = obj(vec![
        ("family", S),
        ("address", S),
        ("prefixLength", S),
        ("scope", S),
        ("scopeId", S),
        ("configSource", S),
    ]);
    let lease = obj(vec![
        ("address", S),
        ("prefixLength", S),
        ("server", S),
        ("router", S),
        ("lifetimeSeconds", S),
    ]);
    let association = || {
        obj(vec![
            ("interface", S),
            ("state", S),
            ("associated", S),
            ("ssid", R),
            ("bssid", R),
            ("frequencyMhz", S),
            ("keyManagement", S),
            ("rssiDbm", S),
            ("linkSpeedMbps", S),
        ])
    };
    let interface = obj(vec![
        ("name", S),
        ("index", S),
        ("kind", S),
        ("type", S),
        ("driver", S),
        ("mtu", S),
        (
            "link",
            obj(vec![
                ("administrativeState", S),
                ("operationalState", S),
                ("carrierState", S),
                ("carrier", S),
                ("onlineState", S),
                ("addressState", S),
            ]),
        ),
        ("hardwareAddress", R),
        ("addresses", arr(address)),
        (
            "dhcp",
            obj(vec![("inferred", S), ("state", S), ("lease", lease)]),
        ),
        ("dns", arr(S)),
        ("wifi", association()),
    ]);
    let route = obj(vec![
        ("family", S),
        ("gateway", S),
        ("interface", S),
        ("interfaceIndex", S),
        ("metric", S),
        ("protocol", S),
        ("protocolId", S),
        ("table", S),
        ("tableId", S),
        ("configSource", S),
    ]);
    let network = obj(vec![
        (
            "interfaces",
            obj(vec![("count", S), ("entries", arr(interface))]),
        ),
        (
            "defaultRoutes",
            obj(vec![("count", S), ("entries", arr(route))]),
        ),
        (
            "dns",
            obj(vec![
                ("linkServers", arr(S)),
                ("resolverServers", arr(S)),
                (
                    "probe",
                    obj(vec![("name", S), ("reachable", S), ("result", S)]),
                ),
            ]),
        ),
        (
            "wifi",
            obj(vec![
                ("interfaces", arr(S)),
                ("associations", arr(association())),
            ]),
        ),
        (
            "capabilities",
            obj(vec![
                ("wifi", obj(vec![("supported", S), ("interfaces", arr(S))])),
                (
                    "bluetooth",
                    obj(vec![("supported", S), ("adapters", arr(S))]),
                ),
                (
                    "cellular",
                    obj(vec![("supported", S), ("interfaces", arr(S))]),
                ),
            ]),
        ),
    ]);

    obj(vec![
        ("schemaVersion", S),
        ("collectedAt", S),
        (
            "release",
            obj(vec![
                ("board", board()),
                ("release", release()),
                ("kernel", kernel()),
            ]),
        ),
        ("system", system),
        ("boot", boot),
        ("journal", journal),
        ("failures", failures),
        ("storage", storage),
        ("time", time()),
        (
            "telemetry",
            obj(vec![("thermal", thermal), ("watchdog", watchdog)]),
        ),
        ("network", network),
    ])
}
