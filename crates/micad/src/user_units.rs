//! The user's own services (`services.units`), kept on DATA state through
//! Base's `mica-unit`: one implementation of where they live, how `[Install]`
//! links them and how the init is told, whether micad or a hand runs it.
//!
//! micad owns what it applied and nothing else. Every unit it installed is
//! named in a record on STATE; a unit the settings no longer declare is
//! removed only when the record names it, so one put there by hand with
//! `mica-unit` stays and is listed as not managed.
//!
//! A unit is started when it becomes enabled and stopped when it stops being
//! so, never on a reconcile that changes nothing about it: the init starts
//! what is enabled at boot, a oneshot is not rerun by every settings write,
//! and a unit a timer runs is not stopped mid-run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use micad_settings::{Init, ServicesSettings, UserUnit};
use serde_json::{Value, json};

use crate::openrc::{Commands, Host};
use crate::reconciler::systemd::UnitControl;

/// Base's command, in mica-systemd and in mica-openrc.
const PROGRAM: &str = "/usr/sbin/mica-unit";
/// The units micad applied and whether each was enabled, on STATE.
const RECORD: &str = "var/lib/mica/services-applied.json";
/// Where the texts are put for `mica-unit install` to copy: made afresh, root's
/// alone, in root's `/run/mica` (`/tmp` is shared on an OpenRC root).
const SCRATCH: &str = "run/mica/units";

/// The user's services on this root.
pub struct UserUnits {
    init: Init,
    /// Everything is under it: `/` on a device, a directory in a test.
    root: PathBuf,
    commands: Arc<dyn Commands>,
    control: Arc<dyn UnitControl>,
}

impl UserUnits {
    pub fn new(
        init: Init,
        root: impl Into<PathBuf>,
        commands: Arc<dyn Commands>,
        control: Arc<dyn UnitControl>,
    ) -> Self {
        Self {
            init,
            root: root.into(),
            commands,
            control,
        }
    }

    /// The host's, through its init.
    pub fn production(init: Init) -> Self {
        let control: Arc<dyn UnitControl> = match init {
            Init::Systemd => Arc::new(crate::reconciler::systemd::Systemd::new()),
            Init::Openrc => Arc::new(crate::openrc::OpenrcUnits::production()),
        };
        Self::new(init, "/", Arc::new(Host), control)
    }

    /// A daemon without host access: no `mica-unit` under a root that has
    /// nothing.
    pub fn unavailable() -> Self {
        Self::new(
            Init::Systemd,
            "/nonexistent",
            Arc::new(Host),
            Arc::new(crate::reconciler::systemd::Systemd::new()),
        )
    }

    /// The init the units are for.
    pub(crate) fn init(&self) -> Init {
        self.init
    }

    /// The init's control the units are driven through.
    pub(crate) fn control(&self) -> &dyn UnitControl {
        self.control.as_ref()
    }

    fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    /// Whether this root has `mica-unit`.
    pub fn available(&self) -> bool {
        self.path(PROGRAM).is_file()
    }

    /// `mica-unit`'s directory.
    fn dir(&self) -> PathBuf {
        match self.init {
            Init::Systemd => self.path("/usr/local/lib/systemd/system"),
            Init::Openrc => self.path("/usr/local/etc"),
        }
    }

    /// Where `name`'s text is.
    fn text_path(&self, name: &str) -> PathBuf {
        match self.init {
            Init::Systemd => self.dir().join(name),
            Init::Openrc => self.dir().join("init.d").join(name),
        }
    }

    /// Where `name`'s conf is (OpenRC).
    fn conf_path(&self, name: &str) -> PathBuf {
        self.dir().join("conf.d").join(name)
    }

    /// Whether the root ships `name`, where `mica-unit` looks.
    pub(crate) fn shipped(&self, name: &str) -> bool {
        let dirs: &[&str] = match self.init {
            Init::Systemd => &[
                "/etc/systemd/system",
                "/usr/lib/systemd/system",
                "/lib/systemd/system",
            ],
            Init::Openrc => &["/etc/init.d"],
        };
        dirs.iter()
            .any(|dir| self.path(dir).join(name).symlink_metadata().is_ok())
    }

    /// The name the init's control knows `name` by: OpenRC's maps a
    /// `.service` onto its script.
    pub(crate) fn control_name(&self, name: &str) -> String {
        match self.init {
            Init::Systemd => name.to_string(),
            Init::Openrc => format!("{name}.service"),
        }
    }

    /// Refuse a write that adds or changes a unit this device cannot apply:
    /// one of the other init, one with no `mica-unit` to keep it, or a name
    /// (or an alias) the root ships.
    ///
    /// # Errors
    ///
    /// Returns the sentence the refusal carries.
    pub fn check_written(
        &self,
        current: &ServicesSettings,
        next: &ServicesSettings,
    ) -> Result<(), String> {
        for (name, unit) in &next.units {
            if current.units.get(name) == Some(unit) {
                continue;
            }
            if unit.kind != self.init {
                return Err(format!(
                    "service {name:?} is for {}; this device runs {}",
                    unit.kind.as_str(),
                    self.init.as_str()
                ));
            }
            if !self.available() {
                return Err(format!(
                    "this root has no {PROGRAM}, which keeps the user's services"
                ));
            }
            let aliases = match self.init {
                Init::Systemd => micad_settings::unit_install_values(&unit.text, "Alias"),
                Init::Openrc => Vec::new(),
            };
            if let Some(shipped) = std::iter::once(name)
                .chain(&aliases)
                .find(|name| self.shipped(name))
            {
                return Err(format!(
                    "{shipped} is shipped by the root; a user service cannot replace it"
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn mica_unit(&self, args: &[&str]) -> Result<String> {
        let output = self.commands.run(PROGRAM, args).await?;
        let line = format!("mica-unit {}", args.join(" "));
        Ok(output.success(&line)?.stdout)
    }

    /// The units in `mica-unit`'s directory and whether each is enabled.
    pub(crate) async fn listed(&self) -> Result<BTreeMap<String, bool>> {
        Ok(self
            .mica_unit(&["list"])
            .await?
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(name, state)| (name.to_string(), state == "enabled"))
            .collect())
    }

    fn read_record(&self) -> BTreeMap<String, bool> {
        let path = self.path(RECORD);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), %err, "the applied services record does not parse; read as empty");
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        }
    }

    fn write_record(&self, record: &BTreeMap<String, bool>) -> Result<()> {
        let text = serde_json::to_string_pretty(record)? + "\n";
        crate::fswrite::write_config_if_changed(&self.path(RECORD), &text, 0o600)?;
        Ok(())
    }

    /// `mica-unit install` `text` as `file` (`--conf` for a conf).
    pub(crate) async fn install(&self, file: &str, text: &str, conf: bool) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

        let scratch = self.path(SCRATCH);
        if let Some(parent) = scratch.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        match std::fs::remove_dir_all(&scratch) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("clear {}", scratch.display()));
            }
            _ => {}
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch)
            .with_context(|| format!("create {}", scratch.display()))?;
        let path = scratch.join(file);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut out| out.write_all(text.as_bytes()))
            .with_context(|| format!("write {}", path.display()))?;
        let path = path.to_string_lossy();
        let installed = if conf {
            self.mica_unit(&["install", "--conf", &path]).await
        } else {
            self.mica_unit(&["install", &path]).await
        };
        let _ = std::fs::remove_dir_all(&scratch);
        installed.map(drop)
    }

    /// Converge the units to `services`; the live state, or every unit that
    /// failed by name.
    ///
    /// # Errors
    ///
    /// No `mica-unit` while units are declared, or a unit that could not be
    /// installed, linked, started, stopped or removed.
    pub async fn apply(&self, services: &ServicesSettings) -> Result<Value> {
        let (declared, refused): (BTreeMap<_, _>, BTreeMap<_, _>) = services
            .units
            .iter()
            .partition(|(_, unit)| unit.kind == self.init);
        let refused: Vec<&String> = refused.into_keys().collect();
        let record = self.read_record();
        if !self.available() {
            if declared.is_empty() {
                return Ok(json!({ "available": false, "refused": refused }));
            }
            bail!("this root has no {PROGRAM}, which keeps the user's services");
        }
        let listed = self.listed().await?;
        let mut applied = BTreeMap::new();
        let mut failed = Vec::new();
        for (name, was) in &record {
            if declared.contains_key(name) {
                continue;
            }
            if let Err(err) = self.remove(name, listed.contains_key(name)).await {
                failed.push(format!("{name}: {err:#}"));
                applied.insert(name.clone(), *was);
            }
        }
        let mut units = serde_json::Map::new();
        for (name, unit) in &declared {
            let was = record.get(*name).copied();
            let linked = listed.get(*name).copied().unwrap_or(false);
            match self.apply_unit(name, unit, was, linked).await {
                Ok(()) => {
                    applied.insert((*name).clone(), unit.enabled);
                    units.insert((*name).clone(), json!({ "enabled": unit.enabled }));
                }
                Err(err) => {
                    // Recorded either way: what was installed is micad's to
                    // remove, and a first apply that failed starts it next
                    // time.
                    applied.insert((*name).clone(), was.unwrap_or(false));
                    units.insert(
                        (*name).clone(),
                        json!({ "enabled": unit.enabled, "error": format!("{err:#}") }),
                    );
                    failed.push(format!("{name}: {err:#}"));
                }
            }
        }
        self.write_record(&applied)
            .context("record the applied services")?;
        if !failed.is_empty() {
            bail!("{}", failed.join("; "));
        }
        Ok(json!({ "available": true, "units": units, "refused": refused }))
    }

    /// Stop `name` and remove it with its links; one already gone (a reset
    /// cleared the directory) is only forgotten.
    async fn remove(&self, name: &str, listed: bool) -> Result<()> {
        if !listed {
            return Ok(());
        }
        // Stopped first, through the init's bus: `mica-unit remove` stops it
        // too, but as a command bounded tighter than a slow stop.
        if let Err(err) = self.control.stop(&self.control_name(name)).await {
            tracing::warn!(unit = name, %err, "stop before removing");
        }
        self.mica_unit(&["remove", name]).await.map(drop)
    }

    async fn apply_unit(
        &self,
        name: &str,
        unit: &UserUnit,
        was: Option<bool>,
        linked: bool,
    ) -> Result<()> {
        let current = |path: PathBuf| std::fs::read_to_string(path).ok();
        let text_changed = current(self.text_path(name)).as_deref() != Some(unit.text.as_str());
        if text_changed {
            self.install(name, &unit.text, false).await?;
        }
        let conf = unit.conf.as_deref().unwrap_or_default();
        let conf_changed = self.init == Init::Openrc
            && current(self.conf_path(name)).as_deref().unwrap_or_default() != conf;
        if conf_changed {
            self.install(&format!("{name}.conf"), conf, true).await?;
        }
        // A changed unit file may say other targets in [Install]: its links
        // are made again.
        if linked && (!unit.enabled || (text_changed && self.init == Init::Systemd)) {
            self.mica_unit(&["disable", name]).await?;
        }
        if unit.enabled && (!linked || (text_changed && self.init == Init::Systemd)) {
            self.mica_unit(&["enable", name]).await?;
        }
        let control = self.control_name(name);
        match (unit.enabled, was) {
            (true, Some(true)) if text_changed || conf_changed => {
                self.control.restart(&control).await?;
            }
            (true, Some(true)) | (false, None | Some(false)) => {}
            (true, None | Some(false)) => self.control.start(&control).await?,
            (false, Some(true)) => self.control.stop(&control).await?,
        }
        Ok(())
    }

    /// Every unit declared or in `mica-unit`'s directory: whether it is
    /// declared and managed here, enabled, and what the init says of it.
    ///
    /// # Errors
    ///
    /// `mica-unit list` failed.
    pub async fn live(&self, services: &ServicesSettings) -> Result<Value> {
        let available = self.available();
        let listed = if available {
            self.listed().await?
        } else {
            BTreeMap::new()
        };
        let mut names: Vec<&String> = services.units.keys().chain(listed.keys()).collect();
        names.sort();
        names.dedup();
        let mut units = Vec::new();
        for name in names {
            let unit = services.units.get(name);
            let managed = unit.is_some_and(|unit| unit.kind == self.init);
            let mut entry = json!({
                "name": name,
                "declared": unit.is_some(),
                "managed": managed,
                "installed": listed.contains_key(name),
                "enabled": listed.get(name).copied().unwrap_or(false),
            });
            if let Some(unit) = unit {
                entry["kind"] = json!(unit.kind.as_str());
                if !managed {
                    entry["refused"] = json!(format!(
                        "for {}; this device runs {}",
                        unit.kind.as_str(),
                        self.init.as_str()
                    ));
                }
            }
            if listed.contains_key(name) {
                entry["active"] = match self.control.active_state(&self.control_name(name)).await {
                    Ok(state) => json!(state),
                    Err(_) => Value::Null,
                };
            }
            units.push(entry);
        }
        Ok(json!({ "init": self.init.as_str(), "available": available, "units": units }))
    }

    /// Refuse a name that is not in `mica-unit`'s directory, before anything
    /// is done to it.
    ///
    /// # Errors
    ///
    /// No such unit, or no `mica-unit`.
    pub async fn require_installed(&self, name: &str) -> Result<()> {
        if !self.available() {
            bail!("this root has no {PROGRAM}, which keeps the user's services");
        }
        if !self.listed().await?.contains_key(name) {
            bail!("no service named {name:?} is installed on this device");
        }
        Ok(())
    }

    /// Start, stop or restart a unit ([`Self::require_installed`] first).
    ///
    /// # Errors
    ///
    /// The init refused.
    pub async fn act(&self, name: &str, verb: Verb) -> Result<()> {
        let control = self.control_name(name);
        match verb {
            Verb::Start => self.control.start(&control).await,
            Verb::Stop => self.control.stop(&control).await,
            Verb::Restart => self.control.restart(&control).await,
        }
    }

    /// The log of an installed unit: its journal, or the RAM log's lines
    /// tagged with its name, which is what an OpenRC script sends to syslog.
    pub fn log_source<'a>(&self, name: &'a str) -> crate::logs::LogSource<'a> {
        crate::logs::LogSource {
            name,
            feature: None,
            unit: name,
            tag: name,
        }
    }
}

/// The three verbs an installed unit takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    Restart,
}

#[cfg(test)]
pub(crate) mod tests;
