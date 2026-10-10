//! The operator's update document, `/mica/config/updates.json`.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use super::*;

/// Where the operator document lives: the update subsystem's
/// occupant of the `/mica/config/` namespace, on the DATA pool that also backs
/// the `/mica/updates` workspace, so one readiness probe gates both.
pub const DEFAULT_UPDATES_PATH: &str = "/mica/config/updates.json";

/// The operator document's schema tag. Optional — a key the
/// document does not name is a key that takes its default — but checked when
/// present, so a `fleet.json` poured into this path is refused rather than
/// read.
pub const UPDATES_SCHEMA_TAG: &str = "mica/update-config/v1";

/// Longest administrative reboot-gate override a policy may allow, and the
/// built-in default. One hour: long enough to carry a maintenance action,
/// short enough that a forgotten override does not stand disarmed for a week.
pub const OVERRIDE_CEILING_SECONDS: u64 = 3600;

/// Key names that would move a trust anchor into an operator document.
pub(super) const ANCHOR_KEYS: [&str; 6] = [
    "trust",
    "signingKeys",
    "signingKeyId",
    "signingKeyIds",
    "rootPath",
    "keyring",
];

/// Why `auto` may not drive with no maintenance window.
pub const AUTO_NEEDS_A_WINDOW: &str = "policy `auto` requires at least one maintenance window: zero windows means \
      `any time`, which for an automatic install means `the moment a bundle lands`";

/// An overridable key: absent, explicitly `null`, or set.
///
/// `Option<Option<T>>` with serde's absent/present split. Both `None` (the
/// key is not there) and `Some(None)` (the key is `null`) resolve to the
/// baked default, and they are still two different values because a status
/// route has to report which one the operator wrote.
pub(super) type Override<T> = Option<Option<T>>;

/// serde's double-option: absent leaves the field at `None` via `default`,
/// present — including `null` — reaches this and becomes `Some(_)`.
pub(super) fn present<'de, D, T>(deserializer: D) -> Result<Override<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

/// The operator document, exactly as parsed — layer 2, and nothing resolved.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesDocument {
    /// Checked against [`UPDATES_SCHEMA_TAG`] when present, and always
    /// written: [`save_updates`] stamps it, so a machine-written document
    /// names its schema even when the one it replaced did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// What the device does on its own. Overrides `update.policy`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Override<UpdateMode>,
    /// Minutes between automatic checks; `0` disables them. Overrides
    /// `update.checkIntervalMinutes`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub check_interval_minutes: Override<u64>,
    /// The time of day the automatic check is anchored to, `HH:MM` UTC.
    /// Absent leaves the check on `checkIntervalMinutes` measured from the
    /// daemon's start. UTC for the reason [`MaintenanceWindow`] is UTC: the
    /// device clock is UTC and `time.timezone` is presentation only.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub check_at: Override<String>,
    /// The core channel this device follows instead of the one its product
    /// bakes. Absent or `null` follows the baked channel.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub core_channel: Override<String>,
    /// What the automatic path does after an install. Read only under
    /// [`UpdateMode::Auto`], which is the only mode that installs. Not an
    /// override: layer 1 carries no default for it.
    #[serde(default)]
    pub reboot_policy: RebootPolicy,
    #[serde(default)]
    pub source: UpdatesSource,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub maintenance: MaintenancePolicy,
    #[serde(default)]
    pub reboot_gate: RebootGatePolicy,
}

/// Online catalog selection and the byte budget for acquired component files.
/// The acquisition workspace and durable metadata locations are fixed by the
/// signed deployment contract; policy cannot redirect them.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesSource {
    /// The update root, ending in `/` (such as `https://res.micaos.dev/update/`):
    /// mica-deploy appends the manifest major it reads. Absent or `null` = the baked
    /// default, which may itself be absent — no online source, so
    /// `check`/`fetch` are refused and the offline import path remains.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Override<String>,
    /// Byte budget for acquired component files (`mica-deploy --max-bytes`).
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}

pub(super) fn default_max_bytes() -> u64 {
    // The operator can raise this limit for larger component sets.
    500_000_000
}

impl Default for UpdatesSource {
    fn default() -> Self {
        Self {
            url: None,
            max_bytes: default_max_bytes(),
        }
    }
}

/// How the device's connectivity is classed. Declared by the operator, not
/// detected: micad has no metering signal to read, and a policy that guessed
/// would be wrong in exactly the deployments that care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkMode {
    /// Unrestricted: catalog checks and component downloads allowed.
    #[default]
    Online,
    /// Metered: catalog checks (KiB) allowed, component downloads (hundreds of
    /// MiB) refused unless `meteredAllowsFetch` says otherwise.
    Metered,
    /// No network use at all: import-only. `check` and `fetch` are refused;
    /// the offline import path is the update channel.
    Offline,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NetworkPolicy {
    #[serde(default)]
    pub mode: NetworkMode,
    /// Permit component downloads on a metered link. Explicitly the exception,
    /// so the metered default is the cheap one.
    #[serde(default)]
    pub metered_allows_fetch: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MaintenancePolicy {
    /// When installs may run. An empty list means "any time" — maintenance
    /// windows are opt-in, because a device with no operator-set window must
    /// still be updatable.
    #[serde(default)]
    pub windows: Vec<MaintenanceWindow>,
}

/// One recurring window, in UTC. UTC rather than local time because the
/// appliance has no trustworthy local-time configuration to read, and a
/// window that silently shifted with a timezone guess would fire in
/// somebody's business hours.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MaintenanceWindow {
    /// Days the window opens on: `mon`..`sun`. Empty means every day.
    #[serde(default)]
    pub days: Vec<String>,
    /// Opening time, `HH:MM` UTC.
    pub start: String,
    /// Closing time, `HH:MM` UTC. A close at or before the open wraps past
    /// midnight into the next day.
    pub end: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RebootGatePolicy {
    /// Health statuses (live-state `health.<component>.status`) that close
    /// the safe-to-reboot gate. The default contract is the single word
    /// `blocking`: an application that must not be interrupted reports
    /// `ReportHealth(component, "blocking", why)` and clears it when done.
    /// `mica-health`'s `degraded` (disk pressure) deliberately does NOT block
    /// — a reboot neither worsens nor is worsened by a full `/var`.
    #[serde(default = "default_blocking_statuses")]
    pub blocking_statuses: Vec<String>,
    /// Longest override TTL this device grants, capped at
    /// [`OVERRIDE_CEILING_SECONDS`] whatever the file says.
    #[serde(default = "default_override_max")]
    pub override_max_seconds: u64,
}

pub(super) fn default_blocking_statuses() -> Vec<String> {
    vec!["blocking".to_string()]
}
pub(super) fn default_override_max() -> u64 {
    OVERRIDE_CEILING_SECONDS
}

impl Default for RebootGatePolicy {
    fn default() -> Self {
        Self {
            blocking_statuses: default_blocking_statuses(),
            override_max_seconds: default_override_max(),
        }
    }
}

impl RebootGatePolicy {
    /// The TTL ceiling actually granted: the file's value, never above the
    /// built-in ceiling — a policy file cannot mint a week-long override.
    pub fn override_ceiling(&self) -> u64 {
        self.override_max_seconds.min(OVERRIDE_CEILING_SECONDS)
    }
}
