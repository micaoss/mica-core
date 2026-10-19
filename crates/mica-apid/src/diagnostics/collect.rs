//! Collecting a snapshot from micad's surfaces.

use crate::settings_api::SettingsApi;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::*;

/// How one section's read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionStatus {
    /// The source answered.
    Ok,
    /// The source answered with an error; the member says which.
    Unavailable,
    /// The source did not answer within its bound, or the deadline had
    /// already passed when its turn came.
    Timeout,
}

impl SectionStatus {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
        }
    }
}

/// What one collection did.
#[derive(Debug, Clone)]
pub struct CollectionReport {
    /// Wall time the whole collection took.
    pub elapsed: Duration,
    /// Per source, how the read ended.
    pub sections: BTreeMap<&'static str, SectionStatus>,
    /// The redaction pass's counts.
    pub redaction: RedactionStats,
}

/// A produced snapshot: the redacted value and the report.
#[derive(Debug, Clone)]
pub struct Collected {
    /// The redacted, versioned snapshot.
    pub snapshot: Value,
    /// How the collection went.
    pub report: CollectionReport,
}

/// Assembles a snapshot from the micad surfaces.
pub struct Collector<'a> {
    pub(super) api: &'a dyn SettingsApi,
    pub(super) deadline: Duration,
    pub(super) section_timeout: Duration,
}

pub(super) use micad_settings::absent;

/// `section[key]`, or an absent member explaining that the section itself
/// was not collected.
pub(super) fn pick(section: &Value, key: &str) -> Value {
    if let Some(member) = section.get(key) {
        member.clone()
    } else {
        let detail = section
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or("the source did not carry this member");
        absent(format!("not collected: {detail}"))
    }
}

/// The tasks in the live-state history whose outcome was not success.
pub(super) fn failed_tasks(state: &Value) -> Value {
    let Some(tasks) = state.get("tasks").and_then(Value::as_array) else {
        return absent("the live-state tree carries no task history");
    };
    let failed: Vec<Value> = tasks
        .iter()
        .filter(|task| {
            task.get("outcome")
                .and_then(Value::as_str)
                .is_some_and(|outcome| outcome != "succeeded")
        })
        .take(MAX_FAILED_TASKS)
        .cloned()
        .collect();
    Value::Array(failed)
}

impl<'a> Collector<'a> {
    /// A collector over `api` on the shipped bounds.
    #[must_use]
    pub fn new(api: &'a dyn SettingsApi) -> Self {
        Self {
            api,
            deadline: COLLECTION_DEADLINE,
            section_timeout: SECTION_TIMEOUT,
        }
    }

    /// Override both bounds, for the tests that prove they are enforced.
    #[cfg(test)]
    #[must_use]
    pub fn with_bounds(mut self, deadline: Duration, section_timeout: Duration) -> Self {
        self.deadline = deadline;
        self.section_timeout = section_timeout;
        self
    }

    /// One section's read under the remaining deadline and the section
    /// timeout, whichever is shorter.
    pub(super) async fn section<F>(&self, started: Instant, read: F) -> (Value, SectionStatus)
    where
        F: Future<Output = anyhow::Result<Value>>,
    {
        let remaining = self.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return (
                absent(format!(
                    "the collection deadline of {:?} passed before this source was read",
                    self.deadline
                )),
                SectionStatus::Timeout,
            );
        }
        let bound = remaining.min(self.section_timeout);
        match tokio::time::timeout(bound, read).await {
            Ok(Ok(value)) => (value, SectionStatus::Ok),
            Ok(Err(err)) => (absent(format!("{err:#}")), SectionStatus::Unavailable),
            Err(_) => (
                absent(format!("no answer within {bound:?}")),
                SectionStatus::Timeout,
            ),
        }
    }

    /// Collect, assemble and redact one snapshot. Never fails: a source that
    /// does not answer is an absent member, and the snapshot is produced
    /// within the deadline whatever the sources do.
    pub async fn collect(&self) -> Collected {
        let started = Instant::now();
        let mut sections = BTreeMap::new();
        let (system, status) = self.section(started, self.api.get_system_info()).await;
        sections.insert("system", status);
        let (telemetry, status) = self.section(started, self.api.get_telemetry()).await;
        sections.insert("telemetry", status);
        let (failures, status) = self.section(started, self.api.get_failure_evidence()).await;
        sections.insert("failures", status);
        let (storage, status) = self.section(started, self.api.get_storage_status()).await;
        sections.insert("storage", status);
        let (time, status) = self.section(started, self.api.get_time_status()).await;
        sections.insert("time", status);
        let (network, status) = self.section(started, self.api.get_observed_network()).await;
        sections.insert("network", status);
        let (state, status) = self.section(started, self.api.get_state("")).await;
        sections.insert("state", status);

        let raw = json!({
            "schemaVersion": SCHEMA_VERSION,
            // Wall-clock time as this appliance has it; the `time` member says
            // whether that clock is disciplined, and `boot.uptime` is the
            // monotonic reference.
            "collectedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "release": {
                "board": pick(&system, "board"),
                "release": pick(&system, "release"),
                "kernel": pick(&system, "kernel"),
            },
            "system": system,
            "boot": {
                "deployment": pick(&system, "deployment"),
                "uptime": pick(&system, "uptime"),
                "reset": pick(&telemetry, "reset"),
                "update": pick(&state, "update"),
            },
            "journal": pick(&failures, "journal"),
            "failures": {
                "units": pick(&failures, "units"),
                "tasks": failed_tasks(&state),
                "health": pick(&state, "health"),
            },
            "storage": storage,
            "time": time,
            "telemetry": {
                "thermal": pick(&telemetry, "thermal"),
                "watchdog": pick(&telemetry, "watchdog"),
            },
            "network": network,
        });
        let (mut snapshot, redaction) = redact_snapshot(raw);
        let elapsed = started.elapsed();
        // The collection record is metadata this module wrote itself; it is
        // added after the pass so the schema above names only evidence.
        if let Some(root) = snapshot.as_object_mut() {
            root.insert(
                "collection".to_string(),
                json!({
                    "deadlineSeconds": self.deadline.as_secs_f64(),
                    "sectionTimeoutSeconds": self.section_timeout.as_secs_f64(),
                    "elapsedMillis": elapsed.as_millis(),
                    "sections": sections
                        .iter()
                        .map(|(name, status)| ((*name).to_string(), Value::from(status.as_str())))
                        .collect::<Map<String, Value>>(),
                    "redaction": {
                        "schemaVersion": REDACTION_SCHEMA_VERSION,
                        "droppedFields": redaction.dropped_fields,
                        "redactedFields": redaction.redacted_fields,
                    },
                }),
            );
        }
        Collected {
            snapshot,
            report: CollectionReport {
                elapsed,
                sections,
                redaction,
            },
        }
    }
}

// ---- the store ------------------------------------------------------------
