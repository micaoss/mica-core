//! The install gate: maintenance windows, deferrals and the reboot override.

use crate::deployment::Status;
use crate::update_codes::{self, CodedReason};
use crate::update_policy::{self, EffectivePolicy, GateVerdict, Selection};
use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use micad_settings::configuration;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

use super::*;

impl UpdateLifecycle {
    /// Installing a staged candidate consumes its pending acquisition state.
    pub async fn installed(&self, status: &Status) {
        let mut machine = self.machine.lock().await;
        if status.state.candidate.is_some() {
            machine.descriptor = None;
            machine.available = None;
        }
        machine.boot_phase = Some(status.phase());
        drop(machine);
        self.record_snapshot().await;
    }

    /// Write the operator's update document.
    ///
    /// Answers the saved document, absent and `null` still distinct.
    ///
    /// # Errors
    pub async fn write_config(&self, sender: &str, patch_json: &str) -> Result<Value, Refusal> {
        let Some(path) = self.policy.path() else {
            // The dry-run store, which was told to read no file. Refused
            // rather than defaulted to `/mica/config/`: a daemon that reads
            // nothing must not write the device's real configuration.
            return Err(Refusal::Unavailable(
                "this daemon has no update policy document".to_string(),
            ));
        };
        let document = configuration::write_updates(path, patch_json).map_err(|err| match err {
            configuration::WriteRefusal::Rejected(err) => Refusal::Invalid(err.to_string()),
            configuration::WriteRefusal::Unreadable(err) => Refusal::Policy(err.to_string()),
            configuration::WriteRefusal::Unwritable(err) => Refusal::Unavailable(err.to_string()),
        })?;
        let rendered = serde_json::to_value(&document).map_err(|err| {
            // Unreachable: the document was just serialized onto the disk by
            // the call above. Reported rather than unwrapped, because a
            // panic here would take the daemon down over a write that
            // succeeded.
            Refusal::Unavailable(format!("the saved document could not be rendered: {err}"))
        })?;
        // Record the actor and path; apid records the authenticated actor too.
        tracing::warn!(sender, path = %path.display(), "update configuration written");
        // A fresh snapshot rather than a note: the policy is re-read here, so
        // the very next state read reports what was just written instead of
        // what the last action saw.
        self.record_snapshot().await;
        Ok(rendered)
    }

    /// Record why an automatic attempt did not proceed.
    pub async fn defer(&self, reason: &str, detail: &str) {
        let code = update_codes::deferral_code(reason);
        if code == update_codes::UNKNOWN {
            tracing::warn!(
                reason,
                detail,
                "automatic pass deferred for a reason outside the published vocabulary; \
                 recorded as `unknown`"
            );
        }
        let now = Utc::now();
        let mut machine = self.machine.lock().await;
        match &mut machine.deferred {
            Some(existing) if existing.reason == code => {
                existing.detail = detail.to_string();
                existing.at = now;
                existing.count = existing.count.saturating_add(1);
            }
            slot => {
                *slot = Some(Deferral {
                    reason: code,
                    detail: detail.to_string(),
                    since: now,
                    at: now,
                    count: 1,
                });
            }
        }
        drop(machine);
        self.record_snapshot().await;
    }

    /// Forget the last deferral: an automatic attempt proceeded.
    pub async fn resume(&self, only: Option<&str>) {
        {
            let mut machine = self.machine.lock().await;
            let matches = machine
                .deferred
                .as_ref()
                .is_some_and(|deferred| only.is_none_or(|reason| deferred.reason == reason));
            if !matches {
                return;
            }
            machine.deferred = None;
        }
        self.record_snapshot().await;
    }

    /// Why the machine must not reboot right now, or `None` when it may.
    /// The bus layer's `Reboot` asks this before it asks systemd.
    pub async fn reboot_refusal(&self) -> Option<String> {
        let verdict = self.gate_verdict().await;
        if verdict.safe {
            return None;
        }
        Some(format!(
            "reboot refused by the safe-to-reboot gate: {}. \
             An administrator can lift a health block with SetRebootOverride \
             (POST /api/v1/update/reboot-override).",
            verdict.reasons.join("; ")
        ))
    }

    /// Why an install must not start right now (maintenance window / policy
    /// file), or `None`. Consulted by the bus layer's `InstallUpdate`.
    pub fn install_refusal(&self) -> Option<String> {
        update_policy::install_refusal(&self.policy.load(), Utc::now())
    }

    /// Arm the administrative reboot-gate override for `seconds`, bounded by
    /// the policy's ceiling. Answers the recorded override as JSON.
    pub async fn set_reboot_override(&self, sender: &str, seconds: u64) -> Result<Value, Refusal> {
        let loaded = self.policy.load();
        let ceiling = loaded.policy.reboot_gate.override_ceiling();
        if seconds == 0 {
            return Err(Refusal::Invalid(
                "override TTL must be at least 1 second".to_string(),
            ));
        }
        if seconds > ceiling {
            return Err(Refusal::Invalid(format!(
                "override TTL must be at most {ceiling} seconds"
            )));
        }
        let record = OverrideRecord {
            expires_at: Utc::now() + chrono::Duration::seconds(seconds as i64),
            requested_by: sender.to_string(),
        };
        // The one log line that must exist for the audit trail: who disarmed
        // the gate, for how long. apid records its own audit event beside it.
        tracing::warn!(
            sender,
            seconds,
            until = %record.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "safe-to-reboot gate override armed"
        );
        let rendered = json!({
            "until": record.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            "requestedBy": record.requested_by,
        });
        self.machine.lock().await.override_record = Some(record);
        self.record_snapshot().await;
        Ok(rendered)
    }

    /// The gate verdict with the current health tree, install flag and
    /// (expired-pruned) override.
    pub(super) async fn gate_verdict(&self) -> GateVerdict {
        let loaded = self.policy.load();
        let health = self.host.health().await;
        let installing = self.installing.load(Ordering::Acquire);
        let override_active = {
            let mut machine = self.machine.lock().await;
            prune_override(&mut machine.override_record);
            machine.override_record.is_some()
        };
        update_policy::evaluate_gate(
            &loaded.policy.reboot_gate,
            &health,
            installing,
            override_active,
        )
    }

    /// Record a refused action without disturbing the machine: the refusal is
    /// the newest fact an operator polling the state needs to see.
    pub(super) async fn record_refusal(&self, action: &str, refusal: &CodedReason) {
        tracing::warn!(
            action,
            code = refusal.code,
            reason = refusal.text,
            "update action refused by policy"
        );
        self.record_snapshot_with(Some(CodedReason::new(
            refusal.code,
            format!("{action} refused: {}", refusal.text),
        )))
        .await;
    }
}

/// Drop an override whose TTL has passed.
pub(super) fn prune_override(record: &mut Option<OverrideRecord>) {
    if record
        .as_ref()
        .is_some_and(|active| active.expires_at <= Utc::now())
    {
        *record = None;
    }
}

pub(super) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The selection an acquisition runs against, or the failure that says the
/// operator document did not load.
pub(super) fn selection_of(policy: &EffectivePolicy) -> Result<&Selection, Failure> {
    policy.selection.as_ref().ok_or_else(|| {
        Failure::Error(CodedReason::new(
            update_codes::POLICY_NOT_LOADED,
            "the update policy document did not load, so there is no source to check",
        ))
    })
}
