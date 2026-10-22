//! The user's apps: bundles unpacked under `/mica/apps/<name>/<version>`,
//! their units kept by `mica-unit`, their data in `/srv/apps/<name>`.
//!
//! micad's record of what is installed is `/mica/apps/apps.json`. Every change
//! is one flow at a time ([`Apps::begin`]); the flows are in `flow`, the
//! image's seeds in `seed` and the update check in `update`.

mod bundle;
mod devices;
mod flow;
mod manifest;
mod render;
mod seed;
mod update;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use micad_settings::Init;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::user_units::UserUnits;
pub use bundle::MAX_ARCHIVE_BYTES;
pub use flow::InstallOptions;
use flow::PrivilegeDelta;
use manifest::Manifest;
pub use manifest::Problem;
pub use update::{CHECK_EVERY, Fetch, HttpFetch};

/// Where apps are unpacked, and micad's record of them.
const APPS_DIR: &str = "mica/apps";
/// The record, in [`APPS_DIR`].
const RECORD: &str = "apps.json";
/// Uploads and downloads, in [`APPS_DIR`].
pub const INCOMING: &str = "/mica/apps/.incoming";
/// The apps' data.
const DATA_DIR: &str = "srv/apps";
/// The most apps installed.
const MAX_APPS: usize = 16;
/// How long a first-run hook may run.
const HOOK_TIMEOUT: Duration = Duration::from_secs(300);
/// The first uid an app's own user gets; the next [`UID_COUNT`] are apps'.
const UID_BASE: u32 = 30000;
/// How many uids apps may have: 30000 to 39999, mica-accounts' range.
const UID_COUNT: u32 = 10000;
/// The Base tool that names apps' accounts in `/etc/passwd` and `/etc/group`.
const ACCOUNTS_TOOL: &str = "/usr/lib/mica/mica-accounts";

/// Why an app request is refused or failed.
#[derive(Debug)]
pub enum AppError {
    /// The bundle breaks these rules.
    Invalid(Vec<Problem>),
    /// Another app flow is running.
    Busy,
    /// No app of that name is installed.
    NotFound(String),
    /// The app is installed at that version already.
    Installed(String),
    /// The request cannot be done as asked.
    Refused(String),
    /// The bundle asks for these privileges beyond the installed version's,
    /// and the request did not accept them.
    Privileges(String, PrivilegeDelta),
    /// It was tried and failed.
    Failed(anyhow::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(problems) => {
                let lines: Vec<String> = problems
                    .iter()
                    .map(|p| format!("{}: {}", p.rule, p.detail))
                    .collect();
                write!(f, "the bundle is refused: {}", lines.join("; "))
            }
            Self::Busy => write!(f, "another app change is running"),
            Self::NotFound(message)
            | Self::Installed(message)
            | Self::Refused(message)
            | Self::Privileges(message, _) => {
                write!(f, "{message}")
            }
            Self::Failed(err) => write!(f, "{err:#}"),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Failed(err)
    }
}

impl From<bundle::Refusal> for AppError {
    fn from(refusal: bundle::Refusal) -> Self {
        match refusal {
            bundle::Refusal::Invalid(problems) => Self::Invalid(problems),
            bundle::Refusal::Io(err) => Self::Failed(err),
        }
    }
}

/// One version of an app as it was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Installed {
    version: String,
    sha256: String,
    /// `upload`, `cli`, `url` or `seed`.
    source: String,
    installed_at: String,
}

/// The outcome of the last flow on an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastResult {
    action: String,
    outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    at: String,
}

/// What the last update check found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked_at: Option<String>,
    /// The version the version file names, with its bundle for this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<update::Offer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// A version whose install failed: not offered again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed: Option<String>,
}

/// One app in the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppEntry {
    current: Installed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<Installed>,
    /// The versions whose first-run hook has run.
    #[serde(default)]
    first_run: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<LastResult>,
    #[serde(default)]
    update: UpdateState,
}

/// micad's record of the installed apps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    apps: BTreeMap<String, AppEntry>,
    /// Each app's own uid, which is its gid too: kept while its data is,
    /// through a remove, until a purge.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    uids: BTreeMap<String, u32>,
}

/// The apps of one root.
pub struct Apps {
    /// Everything is under it: `/` on a device, a directory in a test. None
    /// keeps no apps.
    root: Option<PathBuf>,
    arch: String,
    units: Arc<UserUnits>,
    fetch: Arc<dyn Fetch>,
    /// Where a first-run hook's output is sent too, tagged: `logger`.
    logger: Option<String>,
    hook_timeout: Duration,
    /// The first uid of apps' own users.
    uid_base: u32,
    /// What a first-run hook of an app's own user runs through.
    setpriv: PathBuf,
    /// mica-accounts; a root without it leaves apps' uids unnamed.
    accounts: PathBuf,
    busy: AtomicBool,
}

/// One flow running; the next may start once it is dropped.
pub struct Busy(Arc<Apps>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

/// Have mica-accounts bind `/etc/passwd` and `/etc/group` again from what
/// STATE holds, after a reset cleared the apps' accounts there. A root
/// without it has nothing to rebuild.
pub fn rebuild_accounts() {
    if !Path::new(ACCOUNTS_TOOL).exists() {
        return;
    }
    match std::process::Command::new(ACCOUNTS_TOOL)
        .arg("build")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "mica-accounts build"
        ),
        Err(err) => tracing::warn!(%err, "mica-accounts build"),
    }
}

/// The name of `app`'s own account.
fn account_name(app: &str) -> String {
    format!("mica-app-{app}")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Apps {
    pub fn new(root: impl Into<PathBuf>, units: Arc<UserUnits>, fetch: Arc<dyn Fetch>) -> Self {
        Self {
            root: Some(root.into()),
            arch: manifest::device_arch().to_string(),
            units,
            fetch,
            logger: None,
            hook_timeout: HOOK_TIMEOUT,
            uid_base: UID_BASE,
            setpriv: PathBuf::from(render::SETPRIV),
            accounts: PathBuf::from(ACCOUNTS_TOOL),
            busy: AtomicBool::new(false),
        }
    }

    /// The device's apps, their units through `units`.
    pub fn production(units: Arc<UserUnits>) -> Self {
        Self {
            logger: Some("logger".into()),
            ..Self::new("/", units, Arc::new(HttpFetch))
        }
    }

    /// A daemon that keeps no apps: every change is refused.
    pub fn unavailable() -> Self {
        Self {
            root: None,
            ..Self::new("/", Arc::new(UserUnits::unavailable()), Arc::new(HttpFetch))
        }
    }

    fn root(&self) -> Result<&Path, AppError> {
        self.root
            .as_deref()
            .ok_or_else(|| AppError::Refused("this daemon keeps no apps".into()))
    }

    fn init(&self) -> Init {
        self.units.init()
    }

    fn apps_dir(&self) -> Result<PathBuf, AppError> {
        Ok(self.root()?.join(APPS_DIR))
    }

    fn app_dir(&self, name: &str) -> Result<PathBuf, AppError> {
        Ok(self.apps_dir()?.join(name))
    }

    fn version_dir(&self, name: &str, version: &str) -> Result<PathBuf, AppError> {
        Ok(self.app_dir(name)?.join(version))
    }

    fn data_dir(&self, name: &str) -> Result<PathBuf, AppError> {
        Ok(self.root()?.join(DATA_DIR).join(name))
    }

    /// Where uploads and downloads wait.
    pub fn incoming(&self) -> Result<PathBuf, AppError> {
        Ok(self.root()?.join(INCOMING.trim_start_matches('/')))
    }

    /// Start a flow, unless one is running.
    ///
    /// # Errors
    ///
    /// [`AppError::Busy`].
    pub fn begin(self: &Arc<Self>) -> Result<Busy, AppError> {
        self.root()?;
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AppError::Busy)?;
        Ok(Busy(Arc::clone(self)))
    }

    fn read_record(&self) -> Record {
        let Ok(dir) = self.apps_dir() else {
            return Record::default();
        };
        let path = dir.join(RECORD);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), %err, "the apps record does not parse; read as empty");
                Record::default()
            }),
            Err(_) => Record::default(),
        }
    }

    fn write_record(&self, record: &Record) -> Result<(), AppError> {
        let text = serde_json::to_string_pretty(record).map_err(anyhow::Error::from)? + "\n";
        crate::fswrite::write_config_if_changed(&self.apps_dir()?.join(RECORD), &text, 0o600)?;
        Ok(())
    }

    /// The manifest of `name` at `version`, as unpacked.
    fn manifest_of(&self, name: &str, version: &str) -> Result<Manifest, AppError> {
        Ok(bundle::read_manifest(&self.version_dir(name, version)?)?)
    }

    /// Every unit an installed app owns, with the app.
    pub fn unit_owners(&self) -> BTreeMap<String, String> {
        let mut owners = BTreeMap::new();
        for (name, entry) in self.read_record().apps {
            if let Ok(manifest) = self.manifest_of(&name, &entry.current.version) {
                for (unit, _) in manifest::units_for(&manifest, self.init()) {
                    owners.insert(unit, name.clone());
                }
            }
        }
        owners
    }

    /// Every installed app, or the one named, as the API shows it.
    ///
    /// # Errors
    ///
    /// No app of that name.
    pub async fn view(&self, only: Option<&str>) -> Result<Value, AppError> {
        let record = self.read_record();
        if let Some(name) = only
            && !record.apps.contains_key(name)
        {
            return Err(AppError::NotFound(format!(
                "no app named {name:?} is installed"
            )));
        }
        let seeds = self.read_seed_record();
        let mut apps = Vec::new();
        for (name, entry) in &record.apps {
            if only.is_some_and(|only| only != name) {
                continue;
            }
            apps.push(self.entry_view(name, entry, seeds.get(name)).await);
        }
        Ok(match only {
            Some(_) => apps.pop().unwrap_or(Value::Null),
            None => json!({ "available": self.root.is_some(), "arch": self.arch, "apps": apps }),
        })
    }

    async fn entry_view(&self, name: &str, entry: &AppEntry, seed: Option<&seed::Seeded>) -> Value {
        let manifest = self.manifest_of(name, &entry.current.version).ok();
        let mut units = Vec::new();
        if let Some(manifest) = &manifest {
            for (unit, enable) in manifest::units_for(manifest, self.init()) {
                let active = self
                    .units
                    .control()
                    .active_state(&self.units.control_name(&unit))
                    .await
                    .map_or(Value::Null, Value::from);
                units.push(json!({ "name": unit, "enabled": enable, "active": active }));
            }
        }
        let privileges = manifest.as_ref().map(|m| {
            let root = m.privileges.user == manifest::RunAs::Root;
            json!({
                "user": m.privileges.user,
                "account": (!root).then(|| account_name(name)),
                "uid": if root { None } else { self.read_record().uids.get(name).copied() },
                "capabilities": m.privileges.capabilities,
                "devices": m.privileges.devices,
            })
        });
        let update = manifest.as_ref().and_then(|m| m.update.as_ref());
        let available = entry
            .update
            .latest
            .as_ref()
            .filter(|offer| update::is_available(entry, &offer.version))
            .map(|offer| offer.version.clone());
        json!({
            "name": name,
            "version": entry.current.version,
            "description": manifest.as_ref().map(|m| m.description.clone()).unwrap_or_default(),
            "arch": manifest.as_ref().map(|m| m.arch.clone()).unwrap_or_default(),
            "sha256": entry.current.sha256,
            "source": entry.current.source,
            "installedAt": entry.current.installed_at,
            "previous": entry.previous.as_ref().map(|p| &p.version),
            "units": units,
            "privileges": privileges,
            "last": entry.last,
            "update": update.map(|spec| json!({
                "url": spec.url,
                "auto": spec.auto,
                "available": available,
                "checkedAt": entry.update.checked_at,
                "error": entry.update.error,
            })),
            "seed": seed.map(|seed| json!({
                "version": seed.version,
                "policy": manifest.as_ref().map(|m| m.seed_policy),
            })),
        })
    }
}

#[cfg(test)]
mod tests;
