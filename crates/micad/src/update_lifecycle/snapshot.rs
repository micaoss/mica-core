//! The update state as the live-state tree reports it.

use crate::update_codes::CodedReason;
use crate::update_policy::{self, GateVerdict, LoadedPolicy};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::Ordering;

use super::*;

impl UpdateLifecycle {
    pub(super) async fn record_snapshot(&self) {
        self.record_snapshot_with(None).await;
    }

    /// Build the full `update.lifecycle` entry and hand it to the host.
    pub(super) async fn record_snapshot_with(&self, last_refusal: Option<CodedReason>) {
        let loaded = self.policy.load();
        let health = self.host.health().await;
        let installing = self.installing.load(Ordering::Acquire);
        let mut machine = self.machine.lock().await;
        prune_override(&mut machine.override_record);
        let verdict = update_policy::evaluate_gate(
            &loaded.policy.reboot_gate,
            &health,
            installing,
            machine.override_record.is_some(),
        );
        let entry = render_entry(
            &machine,
            &loaded,
            &verdict,
            installing,
            self.client.unavailable(),
            self.policy.path(),
            last_refusal,
            &self.workspace_root,
        );
        drop(machine);
        self.host.record(entry).await;
    }
}

/// The one place the recorded entry is shaped, so the state precedence —
/// installing over a running client operation over an unready workspace
/// over a failure over a staged descriptor over the boot-derived phase over
/// idle — is written once.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_entry(
    machine: &Machine,
    loaded: &LoadedPolicy,
    gate: &GateVerdict,
    installing: bool,
    client_unavailable: Option<String>,
    policy_path: Option<&std::path::Path>,
    last_refusal: Option<CodedReason>,
    workspace_root: &Path,
) -> Value {
    // `code` is present exactly for the two states that report a FAILURE, and
    // absent for the rest. `ready` and the boot-derived phases
    // carry a `reason` too, but theirs describes a state that is already its
    // own enumerated word — coding "a verified descriptor is staged" would add a
    // second spelling of `ready` and nothing else.
    let (state, reason, code): (&str, Option<String>, Option<&'static str>) = if installing {
        ("installing", None, None)
    } else if let Some(operation) = machine.operation {
        (operation, None, None)
    } else if let Some(unready) = &machine.unready {
        (
            "update-unavailable",
            Some(unready.reason()),
            Some(unready.kind),
        )
    } else if let Some(failed) = &machine.failed {
        ("failed", Some(failed.text.clone()), Some(failed.code))
    } else if machine.descriptor.is_some() {
        (
            "ready",
            Some("a verified descriptor is staged for install".to_string()),
            None,
        )
    } else if let Some((phase, why)) = &machine.boot_phase {
        (*phase, Some(why.clone()), None)
    } else {
        ("idle", None, None)
    };
    let mut entry = serde_json::Map::new();
    entry.insert("state".into(), json!(state));
    if let Some(reason) = reason {
        entry.insert("reason".into(), json!(reason));
    }
    if let Some(code) = code {
        entry.insert("code".into(), json!(code));
    }
    if let Some(refusal) = last_refusal {
        entry.insert("last_refusal".into(), json!(refusal.text));
        entry.insert("last_refusal_code".into(), json!(refusal.code));
    }
    if let Some(available) = &machine.available {
        entry.insert(
            "available".into(),
            json!({
                "deploymentId": available.deployment_id,
                "version": available.version,
            }),
        );
    }
    if let Some(descriptor) = &machine.descriptor {
        entry.insert(
            "deploymentId".into(),
            json!(Path::new(descriptor).file_stem().and_then(|id| id.to_str())),
        );
    }
    if let Some(last_check) = &machine.last_check {
        entry.insert("last_check".into(), json!(last_check));
    }
    // Deferral must be VISIBLE, not silent. A permanently
    // blocking application permanently defers the reboot, which is correct
    // and is also indistinguishable from a stuck update unless the device
    // says so. `since`/`waitedSeconds`/`count` are the "how long" half of
    // that sentence; the state beside it (`ready`, `reboot-required`) is
    // still what the device IS, because a deferral is a fact about the
    // automatic path and not a state of the machine.
    if let Some(deferred) = &machine.deferred {
        entry.insert(
            "deferred".into(),
            json!({
                "reason": deferred.reason,
                "detail": deferred.detail,
                "since": deferred.since.to_rfc3339_opts(SecondsFormat::Secs, true),
                "at": deferred.at.to_rfc3339_opts(SecondsFormat::Secs, true),
                "waitedSeconds": (Utc::now() - deferred.since).num_seconds().max(0),
                "attempts": deferred.count,
            }),
        );
    }
    entry.insert(
        "client".into(),
        match &client_unavailable {
            None => json!({ "available": true }),
            Some(reason) => json!({ "available": false, "reason": reason }),
        },
    );
    // The workspace as the last probe saw it: `status` is the vocabulary
    // the storage surface shares (`ready`, `degraded`, `unavailable`), or
    // `unprobed` before any check/fetch has run. Capacity figures are the
    // DATA pool's, stated once.
    let mut workspace = serde_json::Map::new();
    workspace.insert("root".into(), json!(workspace_root.display().to_string()));
    match (&machine.unready, &machine.workspace) {
        (Some(unready), _) => {
            workspace.insert("status".into(), json!(unready.status));
            workspace.insert("kind".into(), json!(unready.kind));
            workspace.insert("detail".into(), json!(unready.detail));
        }
        (None, Some(report)) => {
            workspace.insert("status".into(), json!("ready"));
            if let Some(report) = report.as_object() {
                for (key, value) in report {
                    if key != "root" {
                        workspace.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (None, None) => {
            workspace.insert("status".into(), json!("unprobed"));
        }
    }
    entry.insert("workspace".into(), Value::Object(workspace));
    let policy = &loaded.policy;
    let selection = policy.selection.as_ref();
    let windows: Vec<Value> = policy
        .maintenance
        .windows
        .iter()
        .map(|window| {
            json!({
                "days": window.days,
                "start": window.start,
                "end": window.end,
            })
        })
        .collect();
    entry.insert(
        "policy".into(),
        json!({
            "file": policy_path.map(|path| path.display().to_string()),
            // The four values the precedence resolves, and `null`
            // for all four when the operator document did not load --
            // `policy_error` below is what distinguishes "unknown" from
            // "unset", and the baked default is deliberately NOT shown here as
            // a stand-in. The baked/operator/effective reading is
            // `GET /api/v1/provisioning/status`.
            "policy": selection.map(|selection| selection.mode.as_str()),
            "checkIntervalMinutes": selection.map(|selection| selection.check_interval_minutes),
            // Layer 2's own, like `rebootPolicy`: it answers even when the
            // selection does not, and `null` is "no anchor", not "unknown".
            "checkAt": policy.check_at.clone(),
            "sourceUrl": selection.and_then(|selection| selection.url.clone()),
            // Layer 2 owns this one outright, so it answers even when the
            // selection does not.
            "rebootPolicy": policy.reboot_policy.as_str(),
            "networkMode": match policy.network.mode {
                crate::update_policy::NetworkMode::Online => "online",
                crate::update_policy::NetworkMode::Metered => "metered",
                crate::update_policy::NetworkMode::Offline => "offline",
            },
            "meteredAllowsFetch": policy.network.metered_allows_fetch,
            "maintenanceWindows": windows,
            "installAllowedNow": update_policy::install_refusal(loaded, Utc::now()).is_none(),
            "blockingStatuses": policy.reboot_gate.blocking_statuses,
            "overrideMaxSeconds": policy.reboot_gate.override_ceiling(),
        }),
    );
    if let Some(error) = &loaded.error {
        entry.insert("policy_error".into(), json!(error));
    }
    let mut gate_entry = gate.to_json();
    if let (Some(record), Some(map)) = (&machine.override_record, gate_entry.as_object_mut()) {
        map.insert(
            "override".into(),
            json!({
                "until": record.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                "requestedBy": record.requested_by,
            }),
        );
    }
    entry.insert("reboot_gate".into(), gate_entry);
    Value::Object(entry)
}
