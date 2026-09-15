//! `/mica/config/` and the baked layer it overrides: the documents, the
//! readers, and the one resolution both callers share.
//!
//! 1. **The baked manifest**, `/usr/share/mica/meta/updates/manifest.json`,
//!    inside the read-only dm-verity root, carries defaults for the source URL,
//!    channel, policy and check interval. Nothing on the device writes it.
//!    Metadata trust keys belong exclusively to the authenticated kernel package.
//! 2. **`/mica/config/updates.json`**, on DATA: operator-owned, and the only
//!    place any of those four is overridden. It also *owns* the keys layer 1
//!    never carries — the windows, the network mode, the workspace paths and
//!    the reboot-gate keys. The independent `/mica/config/fleet.json` document
//!    carries only the fleet overlay.
//! 3. **The running state**, which configures nothing.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a `/mica/config/` document could not be turned into configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists and could not be read. A missing file is not this: it
    /// is the absent case, and the absent case is the baked defaults.
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bytes are not the document.
    #[error("parse {path}: {message}")]
    Parse { path: PathBuf, message: String },
    /// The document names a trust anchor. Its own variant rather than a
    /// parse error, because it is the one refusal that must survive somebody
    /// widening the schema.
    #[error(
        "{path}: `{key}` names a trust anchor, and anchors are baked into the image. \
         The address this device dials is yours to set; what it will accept is not"
    )]
    Anchor { path: PathBuf, key: String },
    /// The document parses and says something the schema cannot mean.
    #[error("{path}: {message}")]
    Validation { path: PathBuf, message: String },
    /// The document validated and could not be put on the disk. Its own
    /// variant because it is the only one that is not about the operator's
    /// input: the request was right and the device failed it.
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ---------------------------------------------------------------------------
// Layer 1: the baked manifest
// ---------------------------------------------------------------------------

/// Where the build bakes the manifest. `/usr/share/mica/` rather
/// than `/etc/`, because nothing on the device may edit it and `/etc` is where
/// an operator reasonably expects an edit to take.
pub const DEFAULT_MANIFEST_PATH: &str = "/usr/share/mica/meta/updates/manifest.json";

/// The baked document's first key, and the one value of it this reader accepts.
pub const MANIFEST_SCHEMA_TAG: &str = "mica/meta/v1";

/// What the device does on its own, and the one key that says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateMode {
    /// The device initiates nothing and no timer arms. Manual check, fetch
    /// and install stay available behind their existing gates, and so does
    /// the offline import: `off` is not "updates disabled", it is "the
    /// device starts nothing".
    Off,
    /// Metadata checks on `checkIntervalMinutes` and nothing else — never
    /// fetches, never installs. The code default, so what a device with
    /// neither layer configured does is what it did before this enum
    /// existed.
    #[default]
    Check,
    /// Checks, then fetches, then installs inside a maintenance window, then
    /// reboots or does not per [`RebootPolicy`]. The driver and every gate
    /// it meets are micad's `update_auto`.
    Auto,
}

impl UpdateMode {
    /// The document's spelling, for the recorded state and for a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Check => "check",
            Self::Auto => "auto",
        }
    }
}

/// What the automatic path does once a bundle is installed and the new slot
/// waits for its first boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RebootPolicy {
    /// Stop at `reboot-required` and wait for an operator. The default,
    /// because it is what makes `auto` safe to recommend to someone who has
    /// not read the design.
    #[default]
    Manual,
    /// Reboot inside the same maintenance window, honouring the
    /// safe-to-reboot gate exactly as `Reboot` does — and never arming its
    /// override.
    Window,
}

impl RebootPolicy {
    /// The document's spelling, for the recorded state and for a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Window => "window",
        }
    }
}

/// The baked document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedManifest {
    pub schema: String,
    pub product: Product,
    pub update: BakedUpdate,
    pub http: BakedHttp,
    pub fleet: BakedFleet,
}

/// Which product an image is. A label: nothing reads it to make a decision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Product {
    pub vendor: String,
    pub model: String,
}

/// The four update defaults.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedUpdate {
    /// Base URL of the published repository. `null` is a value, not an
    /// omission: no default server.
    pub source: Option<String>,
    pub channel: String,
    pub policy: UpdateMode,
    pub check_interval_minutes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedHttp {
    /// Hosts that may receive the configured update credentials in addition
    /// to the baked source's own origin. Empty by default, and adding to it
    /// is a build-time act.
    pub credential_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BakedFleet {
    pub enabled: bool,
    pub url: Option<String>,
}

impl BakedUpdate {
    /// The defaults a device follows when no manifest could be read.
    ///
    /// No source, so nothing can be checked or fetched against a server this
    /// device was never told about; the channel and cadence are the values
    /// the code has always shipped.
    pub fn code_defaults() -> Self {
        Self {
            source: None,
            channel: "stable".to_string(),
            policy: UpdateMode::Check,
            check_interval_minutes: 1440,
        }
    }
}

impl BakedManifest {
    /// The document the reader answers with when there is none to read.
    ///
    /// Empty of everything a device could act on: no source, no trusted key,
    /// no credential host, fleet off. It is a shape, not this device's
    /// configuration, and [`LoadedManifest::error`] is what says so.
    pub fn code_defaults() -> Self {
        Self {
            schema: MANIFEST_SCHEMA_TAG.to_string(),
            product: Product {
                vendor: String::new(),
                model: String::new(),
            },
            update: BakedUpdate::code_defaults(),
            http: BakedHttp {
                credential_hosts: Vec::new(),
            },
            fleet: BakedFleet {
                enabled: false,
                url: None,
            },
        }
    }

    /// Whether a request to `url` may carry the update credentials.
    pub fn credentials_allowed_for(&self, url: &str) -> bool {
        let Some(target) = Origin::parse(url) else {
            return false;
        };
        if self
            .http
            .credential_hosts
            .iter()
            .any(|host| host.eq_ignore_ascii_case(&target.host))
        {
            return true;
        }
        self.update
            .source
            .as_deref()
            .and_then(Origin::parse)
            .is_some_and(|baked| baked == target)
    }
}

/// Scheme, host and port — the three values same-origin compares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    fn parse(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return None,
        };
        // Everything from the first `/`, `?` or `#` on is path, query or
        // fragment and no part of the origin.
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .filter(|authority| !authority.is_empty())?;
        // A credential decision does not guess at userinfo: a URL carrying
        // `user@host` is refused rather than read as `host`.
        if authority.contains('@') {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            // An IPv6 literal's colons are inside the brackets; a `]` after
            // the last colon means there was no port.
            Some((_, after)) if after.contains(']') => (authority, default_port),
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, default_port),
        };
        (!host.is_empty()).then(|| Self {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

/// One load of the baked manifest: the document, and the reason it is not
/// this device's when it is not. Both, never neither.
pub struct LoadedManifest {
    pub manifest: BakedManifest,
    /// `Some` when the file is missing, unreadable, or does not parse or
    /// validate. Unreachable on a device by construction — the build refuses
    /// a manifest this reader would reject and the file is inside the verity
    /// root — so it is a reported condition rather than a handled one.
    pub error: Option<String>,
    path: PathBuf,
}

impl LoadedManifest {
    /// The live-state entry: the whole document, the file it came from, and
    /// the error when there is one.
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.path.display().to_string(),
            "error": self.error,
            "document": self.manifest,
        })
    }
}

/// Read the baked manifest at `path`.
pub fn load_manifest(path: &Path) -> LoadedManifest {
    let fallback = |error: String| LoadedManifest {
        manifest: BakedManifest::code_defaults(),
        error: Some(error),
        path: path.to_path_buf(),
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => return fallback(format!("read {}: {err}", path.display())),
    };
    let manifest = match serde_json::from_str::<BakedManifest>(&raw) {
        Ok(manifest) => manifest,
        Err(err) => return fallback(format!("parse {}: {err}", path.display())),
    };
    if manifest.schema != MANIFEST_SCHEMA_TAG {
        return fallback(format!(
            "{}: schema is `{}`, and this reader knows `{MANIFEST_SCHEMA_TAG}`",
            path.display(),
            manifest.schema
        ));
    }
    if manifest.update.channel.trim().is_empty() {
        return fallback(format!("{}: update.channel is empty", path.display()));
    }
    LoadedManifest {
        manifest,
        error: None,
        path: path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Layer 2: /mica/config/updates.json
// ---------------------------------------------------------------------------

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
const ANCHOR_KEYS: [&str; 6] = [
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
type Override<T> = Option<Option<T>>;

/// serde's double-option: absent leaves the field at `None` via `default`,
/// present — including `null` — reaches this and becomes `Some(_)`.
fn present<'de, D, T>(deserializer: D) -> Result<Override<T>, D::Error>
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
    /// Base URL of the published repository. Absent or `null` = the baked
    /// default, which may itself be absent — no online source, so
    /// `check`/`fetch` are refused and the offline import path remains.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Override<String>,
    /// Release channel to follow (`mica-deploy check --channel`).
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub channel: Override<String>,
    /// Byte budget for acquired component files (`mica-deploy --max-bytes`).
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}

fn default_max_bytes() -> u64 {
    // The operator can raise this limit for larger component sets.
    500_000_000
}

impl Default for UpdatesSource {
    fn default() -> Self {
        Self {
            url: None,
            channel: None,
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

fn default_blocking_statuses() -> Vec<String> {
    vec!["blocking".to_string()]
}
fn default_override_max() -> u64 {
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

impl MaintenanceWindow {
    /// Whether `now` (UTC) falls inside this window.
    ///
    /// A window whose end is at or before its start wraps past midnight; a
    /// window listing no days opens every day.
    pub fn contains(&self, now: DateTime<Utc>) -> bool {
        // Validation ran at load; an unparseable window here (impossible via
        // `load_updates`) simply never matches, which is the closed side.
        let Some(start) = minutes_of_day(&self.start) else {
            return false;
        };
        let Some(end) = minutes_of_day(&self.end) else {
            return false;
        };
        let now_week = minutes_of_week(now);
        let days: Vec<u32> = if self.days.is_empty() {
            (0..7).collect()
        } else {
            self.days.iter().filter_map(|day| day_index(day)).collect()
        };
        for day in days {
            let open = day * 1440 + start;
            let span = if end > start {
                end - start
            } else {
                // Wrapping: 23:00–01:00 is two hours into the next day. Equal
                // start and end reads as a full 24 hours.
                1440 - start + end
            };
            let offset = (now_week + WEEK_MINUTES - open) % WEEK_MINUTES;
            if offset < span {
                return true;
            }
        }
        false
    }
}

/// `HH:MM` → minutes since midnight, or `None` when it is not that.
fn minutes_of_day(clock: &str) -> Option<u32> {
    let (hours, minutes) = clock.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// `mon`..`sun` → 0..6, Monday first (chrono's `num_days_from_monday`).
fn day_index(day: &str) -> Option<u32> {
    Some(match day {
        "mon" => 0,
        "tue" => 1,
        "wed" => 2,
        "thu" => 3,
        "fri" => 4,
        "sat" => 5,
        "sun" => 6,
        _ => return None,
    })
}

/// Minutes into the UTC week (Monday 00:00 = 0) for `now`.
fn minutes_of_week(now: DateTime<Utc>) -> u32 {
    now.weekday().num_days_from_monday() * 1440 + now.hour() * 60 + now.minute()
}

const WEEK_MINUTES: u32 = 7 * 1440;

/// Read the operator document at `path`.
pub fn load_updates(path: &Path) -> Result<UpdatesDocument, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UpdatesDocument::default());
        }
        Err(err) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    // Parsed to a `Value` first so the anchor scan sees every key the
    // document names, including ones a widened schema would accept.
    let value = serde_json::from_str::<Value>(&raw).map_err(|err| ConfigError::Parse {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    if let Some(key) = anchor_key(&value) {
        return Err(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        });
    }
    let document =
        serde_json::from_value::<UpdatesDocument>(value).map_err(|err| ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
    validate(&document).map_err(|message| ConfigError::Validation {
        path: path.to_path_buf(),
        message,
    })?;
    Ok(document)
}

/// The first anchor-shaped key `value` names, at any depth, or `None`.
///
/// Arrays are walked as well as objects: a `trust` block inside a maintenance
/// window would be refused by `deny_unknown_fields` anyway, but this scan is
/// the one that must not have a hole in it.
fn anchor_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if ANCHOR_KEYS
                    .iter()
                    .any(|anchor| anchor.eq_ignore_ascii_case(key))
                {
                    return Some(key.clone());
                }
                if let Some(found) = anchor_key(child) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(anchor_key),
        _ => None,
    }
}

/// Validate what serde cannot: the schema tag when the document names one,
/// that window times parse and days are day names, and that a document
/// *naming* `auto` names a window to install in.
pub fn validate(document: &UpdatesDocument) -> Result<(), String> {
    if let Some(schema) = &document.schema
        && schema != UPDATES_SCHEMA_TAG
    {
        return Err(format!(
            "schema is `{schema}`, and this reader knows `{UPDATES_SCHEMA_TAG}`"
        ));
    }
    for window in &document.maintenance.windows {
        minutes_of_day(&window.start)
            .ok_or_else(|| format!("maintenance window start `{}` is not HH:MM", window.start))?;
        minutes_of_day(&window.end)
            .ok_or_else(|| format!("maintenance window end `{}` is not HH:MM", window.end))?;
        for day in &window.days {
            day_index(day).ok_or_else(|| {
                format!("maintenance window day `{day}` is not mon/tue/wed/thu/fri/sat/sun")
            })?;
        }
    }
    // The document-local half of the rule. The half precedence creates -- a
    // baked `auto` under a document that names no policy -- cannot be seen
    // from here, and is [`EffectivePolicy::auto_window_refusal`].
    if document.policy.flatten() == Some(UpdateMode::Auto)
        && document.maintenance.windows.is_empty()
    {
        return Err(AUTO_NEEDS_A_WINDOW.to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The write
// ---------------------------------------------------------------------------

/// A change to the operator document: the keys the write route names, merged
/// over what is on the disk.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesPatch {
    /// What the device does on its own.
    #[serde(default, deserialize_with = "present")]
    pub policy: Override<UpdateMode>,
    /// Minutes between automatic checks; `0` disables them.
    #[serde(default, deserialize_with = "present")]
    pub check_interval_minutes: Override<u64>,
    /// What the automatic path does after an install. Not an override —
    /// layer 1 bakes no default for it — so it has two states, not three.
    #[serde(default)]
    pub reboot_policy: Option<RebootPolicy>,
    #[serde(default)]
    pub source: Option<UpdatesSourcePatch>,
    /// Replaced whole when named: the object's own serde defaults apply to
    /// the keys the caller leaves out of it.
    #[serde(default)]
    pub network: Option<NetworkPolicy>,
    #[serde(default)]
    pub maintenance: Option<MaintenancePolicy>,
    #[serde(default)]
    pub reboot_gate: Option<RebootGatePolicy>,
}

/// The two overridable keys of `source`, and nothing else it holds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatesSourcePatch {
    /// Where this device dials. `null` returns it to the baked address —
    /// which is a *default*, never a fallback.
    #[serde(default, deserialize_with = "present")]
    pub url: Override<String>,
    #[serde(default, deserialize_with = "present")]
    pub channel: Override<String>,
}

/// Merge `patch` over `document`, key by key.
pub fn apply_patch(mut document: UpdatesDocument, patch: UpdatesPatch) -> UpdatesDocument {
    if let Some(policy) = patch.policy {
        document.policy = Some(policy);
    }
    if let Some(minutes) = patch.check_interval_minutes {
        document.check_interval_minutes = Some(minutes);
    }
    if let Some(reboot_policy) = patch.reboot_policy {
        document.reboot_policy = reboot_policy;
    }
    if let Some(source) = patch.source {
        if let Some(url) = source.url {
            document.source.url = Some(url);
        }
        if let Some(channel) = source.channel {
            document.source.channel = Some(channel);
        }
    }
    if let Some(network) = patch.network {
        document.network = network;
    }
    if let Some(maintenance) = patch.maintenance {
        document.maintenance = maintenance;
    }
    if let Some(reboot_gate) = patch.reboot_gate {
        document.reboot_gate = reboot_gate;
    }
    document
}

/// Why a write did not happen, split by **whose** problem it is.
#[derive(Debug, thiserror::Error)]
pub enum WriteRefusal {
    /// The patch itself, or the document it would produce: not JSON, an
    /// unknown key, a trust anchor, or a value the reader would refuse.
    #[error(transparent)]
    Rejected(ConfigError),
    /// The document on the disk did not load, so there is no base to merge
    /// over. Nothing was written.
    #[error(transparent)]
    Unreadable(ConfigError),
    /// It validated and the device could not store it.
    #[error(transparent)]
    Unwritable(ConfigError),
}

/// Apply `patch_json` to the document at `path` and write the result.
///
/// Answers the document as saved, which is what the operator's layer now says.
///
/// # Errors
pub fn write_updates(path: &Path, patch_json: &str) -> Result<UpdatesDocument, WriteRefusal> {
    let value = serde_json::from_str::<Value>(patch_json).map_err(|err| {
        WriteRefusal::Rejected(ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    })?;
    // Before deserialization and at any depth, exactly as the reader does it:
    // an anchor must be refused by name rather than by whatever
    // `deny_unknown_fields` happens to say on the day the schema grows.
    // The write route is the surface where somebody would
    // *try*.
    if let Some(key) = anchor_key(&value) {
        return Err(WriteRefusal::Rejected(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        }));
    }
    let patch = serde_json::from_value::<UpdatesPatch>(value).map_err(|err| {
        WriteRefusal::Rejected(ConfigError::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    })?;
    let base = load_updates(path).map_err(WriteRefusal::Unreadable)?;
    let document = apply_patch(base, patch);
    save_updates(path, &document).map_err(|err| match err {
        validation @ ConfigError::Validation { .. } => WriteRefusal::Rejected(validation),
        other => WriteRefusal::Unwritable(other),
    })?;
    Ok(document)
}

/// Validate `document` and replace the file at `path` with it, atomically.
///
/// **Validation first, and the same [`validate`] the reader runs.** The
/// `auto`-requires-a-window rule fires here, at the API, rather than hours
/// later at the next check — which is the whole reason the rule has two
/// callers.
pub fn save_updates(path: &Path, document: &UpdatesDocument) -> Result<(), ConfigError> {
    validate(document).map_err(|message| ConfigError::Validation {
        path: path.to_path_buf(),
        message,
    })?;
    let mut stamped = document.clone();
    stamped.schema = Some(UPDATES_SCHEMA_TAG.to_string());
    let mut text = serde_json::to_string_pretty(&stamped).map_err(|err| ConfigError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::other(err),
    })?;
    text.push('\n');
    crate::store::write_atomically(path, &text).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

// ---------------------------------------------------------------------------
// The resolution
// ---------------------------------------------------------------------------

/// What this device follows and where it looks: the four keys layer 1 bakes
/// defaults for, after layer 2 has had its say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The effective source URL. `None` is *no online source configured
    /// anywhere*, not *fall back to the baked one*.
    pub url: Option<String>,
    pub channel: String,
    pub mode: UpdateMode,
    pub check_interval_minutes: u64,
}

/// The workspace values layer 2 owns outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub max_bytes: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            max_bytes: default_max_bytes(),
        }
    }
}

/// The policy after the precedence: what every caller reads.
#[derive(Debug, Clone, Default)]
pub struct EffectivePolicy {
    /// `None` when the operator document exists and did not load.
    ///
    /// Not the baked selection, and not the code default either: a device
    /// whose configuration is unreadable does not know which channel it is
    /// on, and saying so is the whole point. Every action that turns on the
    /// answer is refused while this is `None`.
    pub selection: Option<Selection>,
    pub workspace: Workspace,
    pub network: NetworkPolicy,
    pub maintenance: MaintenancePolicy,
    pub reboot_gate: RebootGatePolicy,
    pub reboot_policy: RebootPolicy,
}

impl EffectivePolicy {
    /// The policy of a device whose operator document did not load.
    pub fn unknown_selection() -> Self {
        Self::default()
    }

    /// Why the automatic path may not install right now, or `None`.
    pub fn auto_window_refusal(&self) -> Option<&'static str> {
        let selection = self.selection.as_ref()?;
        (selection.mode == UpdateMode::Auto && self.maintenance.windows.is_empty())
            .then_some(AUTO_NEEDS_A_WINDOW)
    }
}

/// Resolve layer 2 over layer 1, per key. The one implementation of
/// the precedence; every caller goes through it.
pub fn resolve(baked: &BakedUpdate, document: UpdatesDocument) -> EffectivePolicy {
    EffectivePolicy {
        selection: Some(Selection {
            // `.flatten` is where absent and explicit-`null` become the same
            // answer: both mean "take the baked default". They
            // stay distinguishable in `provisioning_status`, which reports the
            // document rather than the resolution.
            url: document
                .source
                .url
                .flatten()
                .or_else(|| baked.source.clone()),
            channel: document
                .source
                .channel
                .flatten()
                .unwrap_or_else(|| baked.channel.clone()),
            mode: document.policy.flatten().unwrap_or(baked.policy),
            check_interval_minutes: document
                .check_interval_minutes
                .flatten()
                .unwrap_or(baked.check_interval_minutes),
        }),
        workspace: Workspace {
            max_bytes: document.source.max_bytes,
        },
        network: document.network,
        maintenance: document.maintenance,
        reboot_gate: document.reboot_gate,
        reboot_policy: document.reboot_policy,
    }
}

// ---------------------------------------------------------------------------
// Layer 2: /mica/config/fleet.json
// ---------------------------------------------------------------------------

/// Where the desired fleet configuration is poured on the DATA pool.
pub const DEFAULT_FLEET_PATH: &str = "/mica/config/fleet.json";

/// The only fleet document schema this development tree accepts.
const FLEET_SCHEMA_TAG: &str = "mica/fleet-config/v1";

/// The operator's desired fleet overlay, before baked defaults are applied.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetDocument {
    schema: String,
    #[serde(default, deserialize_with = "present_boolean")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "present_boolean")]
    reporting: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    url: Override<String>,
}

/// Keep an omitted boolean optional while rejecting explicit `null`.
fn present_boolean<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map(Some)
}

#[derive(Debug, PartialEq, Eq)]
struct EffectiveFleet {
    enabled: bool,
    reporting: bool,
    url: Option<String>,
}

/// Read the current fleet overlay. Absence selects baked defaults; every
/// present document must parse and validate completely.
fn load_fleet(path: &Path) -> Result<Option<FleetDocument>, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    let value = serde_json::from_str::<Value>(&raw).map_err(|_| ConfigError::Parse {
        path: path.to_path_buf(),
        message: "invalid JSON document".to_string(),
    })?;
    if let Some(key) = anchor_key(&value) {
        return Err(ConfigError::Anchor {
            path: path.to_path_buf(),
            key,
        });
    }
    // Deserialize from the original text, not `value`: serde's struct visitor
    // rejects duplicate fields, while a generic JSON map has already replaced
    // an earlier duplicate by the time it exists.
    let document = serde_json::from_str::<FleetDocument>(&raw).map_err(|_| ConfigError::Parse {
        path: path.to_path_buf(),
        message: "document does not match the fleet configuration schema".to_string(),
    })?;
    if document.schema != FLEET_SCHEMA_TAG {
        return Err(ConfigError::Validation {
            path: path.to_path_buf(),
            message: format!("unsupported schema; expected `{FLEET_SCHEMA_TAG}`"),
        });
    }
    if let Some(Some(value)) = &document.url {
        let valid = url::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        });
        if !valid {
            return Err(ConfigError::Validation {
                path: path.to_path_buf(),
                message: "`url` must be an HTTPS URL without userinfo".to_string(),
            });
        }
    }
    Ok(Some(document))
}

fn effective_fleet(baked: &BakedFleet, document: Option<&FleetDocument>) -> EffectiveFleet {
    let enabled = document
        .and_then(|document| document.enabled)
        .unwrap_or(baked.enabled);
    EffectiveFleet {
        enabled,
        reporting: enabled
            && document
                .and_then(|document| document.reporting)
                .unwrap_or(true),
        url: document
            .and_then(|document| document.url.clone())
            .flatten()
            .or_else(|| baked.url.clone()),
    }
}

// ---------------------------------------------------------------------------
// the read surface
// ---------------------------------------------------------------------------

/// The operator and effective halves of `GET /api/v1/provisioning/status`,
/// read from the production paths.
///
/// See [`provisioning_status_at`], which this is the no-argument form of.
pub fn provisioning_status() -> Result<Value, ConfigError> {
    provisioning_status_at(
        Path::new(DEFAULT_MANIFEST_PATH),
        Path::new(DEFAULT_UPDATES_PATH),
        Path::new(DEFAULT_FLEET_PATH),
    )
}

/// `{ "operator": …, "effective": … }` for the six update fields, resolved
/// through the same reader and the same precedence micad runs on.
///
/// **A key the operator did not write is absent; a key they wrote as `null`
/// is `null`.** Both resolve to the baked default, and they are still two
/// different statements about the document.
pub fn provisioning_status_at(
    manifest: &Path,
    updates: &Path,
    fleet: &Path,
) -> Result<Value, ConfigError> {
    let baked = load_manifest(manifest).manifest;
    let document = load_updates(updates)?;
    let fleet_document = load_fleet(fleet)?;

    let mut operator_update = Map::new();
    if let Some(url) = &document.source.url {
        operator_update.insert("source".into(), json!(url));
    }
    if let Some(channel) = &document.source.channel {
        operator_update.insert("channel".into(), json!(channel));
    }
    if let Some(policy) = &document.policy {
        operator_update.insert("policy".into(), json!(policy));
    }
    let mut operator = Map::new();
    if !operator_update.is_empty() {
        operator.insert("update".into(), Value::Object(operator_update));
    }

    if let Some(document) = &fleet_document {
        let mut operator_fleet = Map::new();
        if let Some(enabled) = document.enabled {
            operator_fleet.insert("enabled".into(), json!(enabled));
        }
        if let Some(reporting) = document.reporting {
            operator_fleet.insert("reporting".into(), json!(reporting));
        }
        if let Some(url) = &document.url {
            operator_fleet.insert("url".into(), json!(url));
        }
        if !operator_fleet.is_empty() {
            operator.insert("fleet".into(), Value::Object(operator_fleet));
        }
    }

    let effective = resolve(&baked.update, document);
    let fleet = effective_fleet(&baked.fleet, fleet_document.as_ref());
    // Unreachable: `resolve` always produces one, and the path that does not
    // is `Err` above. Reported rather than unwrapped, because the whole rule
    // is that nothing substitutes a baked value for an unknown one.
    let selection = effective
        .selection
        .as_ref()
        .ok_or_else(|| ConfigError::Validation {
            path: updates.to_path_buf(),
            message: "the document did not resolve to a selection".to_string(),
        })?;

    Ok(json!({
        "operator": Value::Object(operator),
        "effective": {
            "update": {
                "source": selection.url,
                "channel": selection.channel,
                "policy": selection.mode,
            },
            "fleet": {
                "url": fleet.url,
                "enabled": fleet.enabled,
                "reporting": fleet.reporting,
            },
        },
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fleet_document_rejects_explicit_null_booleans() {
        for document in [
            r#"{ "schema": "mica/fleet-config/v1", "enabled": null }"#,
            r#"{ "schema": "mica/fleet-config/v1", "reporting": null }"#,
        ] {
            assert!(serde_json::from_str::<super::FleetDocument>(document).is_err());
        }
    }

    #[test]
    fn fleet_urls_require_complete_https_authorities_without_normalizing_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.json");
        for url in [
            "https://fleet.example.invalid/path?mode=test#fragment",
            "https://192.0.2.1:8443/report",
            "https://[2001:db8::1]/report?device=1",
        ] {
            std::fs::write(
                &path,
                json!({ "schema": FLEET_SCHEMA_TAG, "url": url }).to_string(),
            )
            .unwrap();
            assert_eq!(
                load_fleet(&path).unwrap().unwrap().url,
                Some(Some(url.to_string()))
            );
        }

        for url in [
            "https://bad host/REJECTED-FLEET-SENTINEL",
            "https://[]/REJECTED-FLEET-SENTINEL",
            "https://REJECTED-FLEET-SENTINEL@fleet.example/path",
        ] {
            std::fs::write(
                &path,
                json!({ "schema": FLEET_SCHEMA_TAG, "url": url }).to_string(),
            )
            .unwrap();
            let error = load_fleet(&path).unwrap_err().to_string();
            assert!(!error.contains("REJECTED-FLEET-SENTINEL"));
        }
    }

    #[test]
    fn fleet_resolution_applies_baked_fallbacks_and_the_reporting_gate() {
        let baked = super::BakedFleet {
            enabled: true,
            url: Some("https://baked.example/fleet".to_string()),
        };
        assert_eq!(
            super::effective_fleet(&baked, None),
            super::EffectiveFleet {
                enabled: true,
                reporting: true,
                url: Some("https://baked.example/fleet".to_string()),
            }
        );

        let disabled = super::FleetDocument {
            schema: super::FLEET_SCHEMA_TAG.to_string(),
            enabled: Some(false),
            reporting: Some(true),
            url: Some(None),
        };
        assert_eq!(
            super::effective_fleet(&baked, Some(&disabled)),
            super::EffectiveFleet {
                enabled: false,
                reporting: false,
                url: Some("https://baked.example/fleet".to_string()),
            }
        );
    }

    #[test]
    fn baked_manifest_keeps_metadata_anchors_in_the_authenticated_boot_policy() {
        let mut value = serde_json::to_value(super::BakedManifest::code_defaults()).unwrap();
        value.as_object_mut().unwrap().remove("trust");
        assert!(serde_json::from_value::<super::BakedManifest>(value.clone()).is_ok());
        value["trust"] = serde_json::json!({"signingKeys": [], "signingKeyIds": []});
        assert!(serde_json::from_value::<super::BakedManifest>(value).is_err());
    }

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn update_sources_reject_removed_metadata_path_options() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        for key in ["repoDir", "statePath"] {
            std::fs::write(&path, json!({"source":{key:"/tmp/metadata"}}).to_string()).unwrap();
            assert!(
                load_updates(&path).is_err(),
                "removed option accepted: {key}"
            );
        }
    }

    /// A document with something in every shape the write has to preserve:
    /// an override that is set, an override cleared to `null`, an override
    /// left absent, and a key layer 2 owns outright.
    fn seeded(path: &Path) {
        std::fs::write(
            path,
            r#"{
              "schema": "mica/update-config/v1",
              "policy": "check",
              "checkIntervalMinutes": null,
              "source": { "channel": "beta", "maxBytes": 123456789 },
              "maintenance": { "windows": [ { "days": ["mon"], "start": "02:00", "end": "04:00" } ] }
            }"#,
        )
        .expect("seed the document");
    }

    #[test]
    fn a_saved_document_loads_back_with_absent_and_null_still_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        seeded(&path);

        let before = load_updates(&path).expect("the seed loads");
        save_updates(&path, &before).expect("it saves");
        let after = load_updates(&path).expect("it loads back");

        // `policy` was set, `checkIntervalMinutes` was an explicit `null` and
        // `source.url` was never named. A save that flattened the last two
        // into each other would rewrite what the operator said.
        assert_eq!(after.policy, Some(Some(UpdateMode::Check)));
        assert_eq!(after.check_interval_minutes, Some(None));
        assert_eq!(after.source.url, None);
        assert_eq!(after.source.channel, Some(Some("beta".to_string())));
        assert_eq!(after.source.max_bytes, 123_456_789);
        assert_eq!(after.maintenance.windows.len(), 1);

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["checkIntervalMinutes"], Value::Null);
        assert!(
            !written.as_object().unwrap().contains_key("policy")
                || written["policy"] == json!("check")
        );
        assert!(
            written["source"].as_object().unwrap().get("url").is_none(),
            "a key the operator never wrote must not appear: {written}"
        );
    }

    #[test]
    fn a_saved_document_names_its_schema_even_when_the_one_it_replaced_did_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();

        write_updates(&path, r#"{ "policy": "check" }"#).expect("the write lands");

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["schema"], json!(UPDATES_SCHEMA_TAG));
    }

    #[test]
    fn a_saved_document_is_mode_0600_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        write_updates(&path, r#"{ "policy": "off" }"#).expect("the write lands");

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "the namespace's documents are 0600"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("updates.json")],
            "the rename consumed the temporary sibling"
        );
    }

    #[test]
    fn a_patch_changes_the_keys_it_names_and_leaves_every_other_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        seeded(&path);

        let saved = write_updates(&path, r#"{ "source": { "channel": "stable" } }"#)
            .expect("the write lands");

        assert_eq!(saved.source.channel, Some(Some("stable".to_string())));
        // Everything the patch did not name survived, including the keys no
        // console renders.
        assert_eq!(saved.policy, Some(Some(UpdateMode::Check)));
        assert_eq!(saved.check_interval_minutes, Some(None));
        assert_eq!(saved.source.max_bytes, 123_456_789);
        assert_eq!(saved.maintenance.windows.len(), 1);
    }

    #[test]
    fn an_explicit_null_clears_an_override_and_an_absent_key_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        seeded(&path);

        let saved =
            write_updates(&path, r#"{ "source": { "url": null } }"#).expect("the write lands");

        // `null` is the operator saying "take the baked address again", and
        // it is recorded as `null` rather than as absence.
        assert_eq!(saved.source.url, Some(None));
        assert_eq!(saved.source.channel, Some(Some("beta".to_string())));
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["source"]["url"], Value::Null);
    }

    #[test]
    fn a_patch_may_re_point_the_device_at_another_address() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        seeded(&path);

        write_updates(
            &path,
            r#"{ "source": { "url": "https://updates.example/repo" } }"#,
        )
        .expect("the write lands");

        let baked = BakedUpdate {
            source: Some("https://baked.example/repo".to_string()),
            ..BakedUpdate::code_defaults()
        };
        let effective = resolve(&baked, load_updates(&path).unwrap());
        assert_eq!(
            effective.selection.unwrap().url.as_deref(),
            Some("https://updates.example/repo"),
            "the override wins over the baked default"
        );
    }

    #[test]
    fn auto_with_no_window_is_refused_at_the_write_and_the_document_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = write_updates(&path, r#"{ "policy": "auto" }"#)
            .expect_err("`auto` with no window is not a document this device may hold");

        assert!(
            matches!(err, WriteRefusal::Rejected(_)),
            "the operator's input is what was wrong: {err}"
        );
        assert!(
            err.to_string().contains(AUTO_NEEDS_A_WINDOW),
            "the refusal states the rule: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a refused write replaces nothing"
        );
    }

    #[test]
    fn auto_with_a_window_in_the_same_patch_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        std::fs::write(&path, r#"{ "policy": "off" }"#).unwrap();

        let saved = write_updates(
            &path,
            r#"{ "policy": "auto",
                 "maintenance": { "windows": [ { "start": "02:00", "end": "04:00" } ] } }"#,
        )
        .expect("the rule is about the resulting document, not about the order of two writes");

        assert_eq!(saved.policy, Some(Some(UpdateMode::Auto)));
    }

    #[test]
    fn a_patch_naming_a_trust_anchor_is_refused_by_that_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        // At the top level, nested inside an object the schema does have, and
        // inside an array: the scan must have no hole, because the write
        // route is the surface where somebody would try.
        let patches = [
            r#"{ "trust": { "signingKeys": ["k"] } }"#,
            r#"{ "signingKeys": ["k"] }"#,
            r#"{ "signingKeyId": "abc" }"#,
            r#"{ "signingKeyIds": ["abc"] }"#,
            r#"{ "source": { "rootPath": "/tmp/root.json" } }"#,
            r#"{ "source": { "keyring": "/tmp/keys" } }"#,
            r#"{ "maintenance": { "windows": [ { "start": "02:00", "end": "04:00", "keyring": "x" } ] } }"#,
        ];
        for patch in patches {
            let err = write_updates(&path, patch).expect_err("an anchor is not the operator's");
            let WriteRefusal::Rejected(ConfigError::Anchor { key, .. }) = &err else {
                panic!("{patch} must be refused as an anchor, was {err}");
            };
            assert!(
                patch.contains(key.as_str()),
                "the refusal names the key it found: {key} not in {patch}"
            );
            assert!(
                err.to_string().contains(key),
                "the message carries the key: {err}"
            );
            assert!(!path.exists(), "a refused write creates nothing");
        }
    }

    #[test]
    fn a_patch_naming_a_key_this_document_does_not_have_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        seeded(&path);
        let before = std::fs::read_to_string(&path).unwrap();

        for (patch, key) in [
            (r#"{ "source": { "repoDir": "/tmp/mirror" } }"#, "repoDir"),
            (r#"{ "source": { "maxBytes": 1 } }"#, "maxBytes"),
            (r#"{ "autoCheck": { "intervalMinutes": 60 } }"#, "autoCheck"),
            (r#"{ "schema": "mica/update-config/v1" }"#, "schema"),
        ] {
            let err = write_updates(&path, patch).expect_err("{patch} is not a key of the patch");
            assert!(
                err.to_string().contains(key),
                "the refusal names the offending field: {err}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }
    }

    #[test]
    fn a_document_that_does_not_load_is_not_patched_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = write_updates(&path, r#"{ "policy": "off" }"#)
            .expect_err("there is no base to merge over");

        assert!(
            matches!(err, WriteRefusal::Unreadable(_)),
            "the file is what is wrong, not the request: {err}"
        );
        assert!(err.to_string().contains("updates.json"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a blind overwrite would drop keys the operator cannot see"
        );
    }

    #[test]
    fn a_write_into_a_missing_namespace_refuses_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // `/mica/config/` absent is the DATA medium not mounted, so a
        // device that created it would write the operator's channel onto the
        // root filesystem where the next boot would not look.
        let path = dir.path().join("not-mounted").join("updates.json");

        let err = write_updates(&path, r#"{ "policy": "off" }"#).expect_err("nowhere to write");

        assert!(matches!(err, WriteRefusal::Unwritable(_)), "{err}");
        assert!(!dir.path().join("not-mounted").exists());
    }

    #[test]
    fn a_patch_that_is_not_json_is_the_requests_fault() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        let err = write_updates(&path, "not json").expect_err("not a patch");
        assert!(matches!(err, WriteRefusal::Rejected(_)), "{err}");
    }
}
