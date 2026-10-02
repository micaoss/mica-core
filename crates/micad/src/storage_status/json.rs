//! The storage status as the live-state tree reports it.

use serde_json::{Value as Json, json};

use super::*;

/// The lifecycle decisions that must be EXPLICIT.
pub(super) const LIFECYCLE: &[(&str, &str)] = &[
    ("backupRestore", "unsupported"),
    ("offlineRepair", "unsupported"),
    ("dataPreservingReplacement", "unsupported"),
    ("factoryReset", "unsupported"),
    ("secureErase", "unsupported"),
    ("encryption", "unsupported"),
    ("removableMedia", "unsupported"),
];

/// `evidence` rendered as the JSON the bus method serves.
///
/// A tier the board does not have is present in the array with
/// `"present": false` rather than omitted, and a metric the device does not
/// export is absent rather than zero.
#[must_use]
pub fn status_json(evidence: &StorageEvidence, pressure: &PressureTracker) -> Json {
    let tiers: Vec<Json> = TIERS
        .iter()
        .map(|spec| tier_json(spec, evidence.tiers.get(spec.name), pressure))
        .collect();
    let media: Vec<Json> = evidence.media.iter().map(medium_json).collect();
    let data = evidence.data_tier();
    // The binds are classified against the DATA tier's CURRENT pressure, and
    // reading it here rather than re-observing keeps one poll's verdicts
    // consistent: `tier_json` above already advanced the hysteresis state for
    // this observation, so asking the tracker again would be a second
    // reading of the same sample.
    let data_pressure = pressure.current(DATA_TIER);
    let binds: Vec<Json> = BINDS
        .iter()
        .map(|spec| bind_json(spec, evidence.binds.get(spec.name), data, data_pressure))
        .collect();
    json!({
        "tiers": tiers,
        // One filesystem, multiple namespaces. This member is not a second tier
        // list: every filesystem capacity number for the binds is the DATA tier's, and
        // saying so here is what stops a reader adding them together.
        "namespaces": {
            "sharedCapacityTier": DATA_TIER,
            "detail": "/mica, /srv and /mica/containers bind directories of one DATA filesystem; capacity is reported once on the data tier, with independent project accounting; system/user and container limits are zero (unlimited), while variable data is bounded",
            "binds": binds,
            "directories": DATA_DIRECTORIES.map(|name| json!({
                "name": name,
                "usedBytes": evidence.directory_bytes.get(name),
                "project": match name { "mica" | "srv" => Some(100), "cache" | "tmp" | "var" => Some(101), "containers" => Some(102), _ => None },
            })),
            "projectQuotas": evidence.project_quotas,
        },
        "media": media,
        "policy": {
            "warningPercent": WARNING_ENTER_PERCENT,
            "warningClearPercent": WARNING_CLEAR_PERCENT,
            "criticalPercent": CRITICAL_ENTER_PERCENT,
            "criticalClearPercent": CRITICAL_CLEAR_PERCENT,
            "watchedTiers": [DATA_TIER],
        },
        "lifecycle": LIFECYCLE
            .iter()
            .map(|(name, decision)| ((*name).to_string(), Json::from(*decision)))
            .collect::<serde_json::Map<String, Json>>(),
    })
}

pub(super) fn tier_json(
    spec: &TierSpec,
    evidence: Option<&TierEvidence>,
    pressure: &PressureTracker,
) -> Json {
    let mut root = serde_json::Map::new();
    root.insert("name".to_string(), json!(spec.name));
    root.insert("role".to_string(), json!(spec.role));
    root.insert(
        "partitionLabel".to_string(),
        json!(
            evidence
                .and_then(|evidence| evidence.partition_label.as_deref())
                .unwrap_or(spec.partition_label)
        ),
    );
    if let Some(mount) = spec.mount {
        root.insert("expectedMount".to_string(), json!(mount));
    }
    let Some(evidence) = evidence else {
        root.insert("present".to_string(), json!(false));
        root.insert(
            "detail".to_string(),
            json!(format!(
                "no partition named `{}` on this board",
                spec.partition_label
            )),
        );
        return Json::Object(root);
    };
    root.insert("present".to_string(), json!(true));
    if let Some(device) = &evidence.device {
        root.insert("device".to_string(), json!(device));
    }
    if let Some(bytes) = evidence.partition_bytes {
        root.insert("partitionBytes".to_string(), json!(bytes));
    }
    root.insert("mounted".to_string(), json!(evidence.mount.is_some()));
    if let Some(mount) = &evidence.mount {
        root.insert("mount".to_string(), json!(mount.mount));
        root.insert("filesystem".to_string(), json!(mount.fstype));
        root.insert("readOnly".to_string(), json!(mount.read_only));
    }
    if let Some(space) = evidence.space {
        root.insert(
            "space".to_string(),
            json!({
                "totalBytes": space.total,
                "usedBytes": space.used,
                "freeBytes": space.free,
                "reservedBytes": space.reserved,
                "usedPercent": space.used_percent(),
            }),
        );
        if spec.name == DATA_TIER {
            root.insert(
                "pressure".to_string(),
                json!(pressure.observe(spec.name, space.used_percent()).as_str()),
            );
        }
    }
    root.insert(
        "check".to_string(),
        match &evidence.check {
            Some(check) => json!({
                "unit": check.unit,
                "activeState": check.active_state,
                "result": check.result,
                "exitStatus": check.exit_status,
            }),
            // Not "clean": a tier with no fsck unit was never checked, and
            // there is no history anywhere that says otherwise.
            None => json!({ "recorded": false }),
        },
    );
    Json::Object(root)
}

/// One bind namespace rendered.
///
/// Deliberately carries NO capacity of its own: `/mica` and `/srv` are two
/// views of the DATA filesystem, and a `space` object here would be the same
/// bytes reported a second and third time.
pub(super) fn bind_json(
    spec: &BindSpec,
    evidence: Option<&BindEvidence>,
    data: Option<&TierEvidence>,
    pressure: Pressure,
) -> Json {
    let mut root = serde_json::Map::new();
    root.insert("name".to_string(), json!(spec.name));
    root.insert("mount".to_string(), json!(spec.mount));
    root.insert("source".to_string(), json!(spec.source));
    root.insert("owner".to_string(), json!(spec.owner));
    let Some(evidence) = evidence else {
        root.insert("readiness".to_string(), json!(Readiness::Unknown.as_str()));
        root.insert(
            "detail".to_string(),
            json!("this daemon observed no mount table"),
        );
        return Json::Object(root);
    };
    let readiness = classify_readiness(evidence, data, pressure);
    root.insert("readiness".to_string(), json!(readiness.as_str()));
    root.insert("mounted".to_string(), json!(evidence.mount.is_some()));
    if let Some(mount) = &evidence.mount {
        root.insert("device".to_string(), json!(mount.device));
        root.insert("readOnly".to_string(), json!(mount.read_only));
        // The contract's first question, answered as its own member rather
        // than folded into the verdict: a writer that falls back to another
        // filesystem is the failure that matters, so "is this actually
        // DATA?" has to be legible on its own.
        root.insert(
            "sourceOnData".to_string(),
            json!(data.and_then(|tier| tier.device.as_deref()) == Some(mount.device.as_str())),
        );
    }
    root.insert(
        "sourceMatchesNamespace".to_string(),
        json!(evidence.mount.as_ref().is_some_and(mount_matches_namespace)),
    );
    if let Some(is_directory) = evidence.source_is_directory {
        root.insert("sourceIsDirectory".to_string(), json!(is_directory));
    }
    if let Some(probe) = &evidence.probe {
        root.insert(
            "probe".to_string(),
            match probe {
                ProbeOutcome::Passed => json!({ "attempted": true, "passed": true }),
                ProbeOutcome::Failed(error) => {
                    json!({ "attempted": true, "passed": false, "error": error })
                }
                // Never `passed: true`. A probe that did not run is the one
                // thing this member must not be mistaken for.
                ProbeOutcome::NotAttempted(reason) => {
                    json!({ "attempted": false, "reason": reason })
                }
            },
        );
    }
    Json::Object(root)
}

pub(super) fn medium_json(medium: &MediumEvidence) -> Json {
    let mut root = serde_json::Map::new();
    root.insert("name".to_string(), json!(medium.name));
    root.insert("kind".to_string(), json!(medium.kind));
    if let Some(bytes) = medium.size_bytes {
        root.insert("sizeBytes".to_string(), json!(bytes));
    }
    if let Some(model) = &medium.model {
        root.insert("model".to_string(), json!(model));
    }
    if let Some(rotational) = medium.rotational {
        root.insert("rotational".to_string(), json!(rotational));
    }
    root.insert("health".to_string(), health_json(&medium.health));
    Json::Object(root)
}

pub(super) fn health_json(health: &MediaHealth) -> Json {
    match health {
        MediaHealth::Emmc {
            life_time_raw,
            pre_eol_raw,
        } => {
            let estimates: Vec<Json> = life_time_raw
                .split_whitespace()
                .map(|field| match parse_life_time(field) {
                    Some(bucket) => json!({
                        "raw": field,
                        "usedPercentMin": bucket.0,
                        "usedPercentMax": bucket.1,
                    }),
                    None => json!({ "raw": field, "detail": "the device does not define this estimate" }),
                })
                .collect();
            json!({
                "supported": true,
                "source": "sysfs mmc life_time / pre_eol_info",
                // The raw registers travel with the normalization so support
                // can read what the device actually said, not only what this
                // module made of it.
                "raw": { "lifeTime": life_time_raw, "preEolInfo": pre_eol_raw },
                "lifetimeEstimates": estimates,
                "preEol": pre_eol_raw.as_deref().map(parse_pre_eol),
            })
        }
        MediaHealth::Unsupported(reason) => json!({ "supported": false, "reason": reason }),
    }
}
