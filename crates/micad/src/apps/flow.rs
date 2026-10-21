//! Install, upgrade, roll back and remove an app.

use std::path::Path;

use anyhow::{Context, bail};
use micad_settings::{Init, ServicesSettings};
use serde_json::{Value, json};

use super::bundle;
use super::manifest::{self, Manifest, Problem};
use super::{AppEntry, AppError, Apps, Installed, LastResult, MAX_APPS, Record, now};
use crate::user_units::Verb;

/// The most bytes of a first-run hook's output kept.
const HOOK_LOG_BYTES: usize = 64 * 1024;
/// The first-run hook's output, in the app's directory.
const HOOK_LOG: &str = "first-run.log";
/// The environment file, in the app's directory.
const ENV_FILE: &str = "app.env";
/// The `ActiveState` of a unit that did not start.
const FAILED_STATE: &str = "failed";

/// What an install asks for besides the bundle.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Install the version that is installed again.
    pub reinstall: bool,
    /// Refuse a bundle of an app that is not installed.
    pub upgrade: bool,
}

impl Apps {
    /// Check `archive` before it is taken: its manifest, its units and what is
    /// installed. Nothing is unpacked.
    ///
    /// # Errors
    ///
    /// Every rule the bundle breaks, or the reason it cannot be installed.
    pub async fn admit(
        &self,
        archive: &Path,
        options: InstallOptions,
        services: &ServicesSettings,
    ) -> Result<Manifest, AppError> {
        self.root()?;
        let manifest = bundle::peek_manifest(archive)?;
        let mut problems = manifest::check_fields(&manifest, self.init(), &self.arch);
        if problems.is_empty() {
            problems.extend(self.unit_problems(&manifest, services).await?);
        }
        let record = self.read_record();
        match record.apps.get(&manifest.name) {
            None if options.upgrade => {
                return Err(AppError::NotFound(format!(
                    "no app named {:?} is installed to upgrade",
                    manifest.name
                )));
            }
            None if record.apps.len() >= MAX_APPS => {
                problems.push(Problem::new(
                    "too_many",
                    format!("{MAX_APPS} apps are installed already"),
                ));
            }
            Some(entry) if entry.current.version == manifest.version && !options.reinstall => {
                return Err(AppError::Installed(format!(
                    "{} {} is installed; reinstall to install it again",
                    manifest.name, manifest.version
                )));
            }
            _ => {}
        }
        if !problems.is_empty() {
            return Err(AppError::Invalid(problems));
        }
        Ok(manifest)
    }

    /// The units of `manifest` another owner has: the root, `services.units`,
    /// another app or a hand.
    async fn unit_problems(
        &self,
        manifest: &Manifest,
        services: &ServicesSettings,
    ) -> Result<Vec<Problem>, AppError> {
        let ours = manifest::units_for(manifest, self.init());
        if ours.is_empty() {
            return Ok(Vec::new());
        }
        if !self.units.available() {
            return Err(AppError::Refused(
                "this root has no mica-unit, which keeps an app's units".into(),
            ));
        }
        let owners = self.unit_owners();
        let listed = self.units.listed().await?;
        let mut problems = Vec::new();
        for (unit, _) in ours {
            let owner = owners.get(&unit);
            if self.units.shipped(&unit) {
                problems.push(Problem::new(
                    "unit_shipped_by_root",
                    format!("{unit} is shipped by the root"),
                ));
            } else if services.units.contains_key(&unit) {
                problems.push(Problem::new(
                    "unit_taken",
                    format!("{unit} is a service in services.units"),
                ));
            } else if let Some(owner) = owner.filter(|owner| **owner != manifest.name) {
                problems.push(Problem::new(
                    "unit_taken",
                    format!("{unit} is app {owner}'s"),
                ));
            } else if owner.is_none() && listed.contains_key(&unit) {
                problems.push(Problem::new(
                    "unit_taken",
                    format!("{unit} is installed already"),
                ));
            }
        }
        Ok(problems)
    }

    /// Install or upgrade from `archive`, recorded as from `source`: check it
    /// again, unpack it, switch to it, and on a failure go back to what was
    /// there.
    ///
    /// # Errors
    ///
    /// Refused as [`Self::admit`] refuses, or failed (and rolled back).
    pub async fn install(
        &self,
        archive: &Path,
        source: &str,
        options: InstallOptions,
        services: &ServicesSettings,
    ) -> Result<Value, AppError> {
        let manifest = self.admit(archive, options, services).await?;
        let name = manifest.name.clone();
        let version = manifest.version.clone();
        let app_dir = self.app_dir(&name)?;
        std::fs::create_dir_all(&app_dir)
            .with_context(|| format!("create {}", app_dir.display()))?;
        let mut record = self.read_record();
        let old = record.apps.get(&name).cloned();
        let partial = app_dir.join(format!(".{version}.partial"));
        let _ = std::fs::remove_dir_all(&partial);
        let unpacked = {
            let (archive, partial) = (archive.to_path_buf(), partial.clone());
            let (init, arch) = (self.init(), self.arch.clone());
            tokio::task::spawn_blocking(move || {
                ensure_room(&archive, &partial)?;
                bundle::unpack(&archive, &partial, init, &arch)
            })
            .await
            .context("unpack the bundle")?
        };
        let unpacked = match unpacked {
            Ok(unpacked) if unpacked.manifest == manifest => unpacked,
            other => {
                let _ = std::fs::remove_dir_all(&partial);
                if old.is_none() {
                    let _ = std::fs::remove_dir(&app_dir);
                }
                return Err(match other {
                    Err(refusal) => refusal.into(),
                    Ok(_) => AppError::Invalid(vec![Problem::new(
                        "manifest",
                        "the bundle changed while it was installed",
                    )]),
                });
            }
        };
        sync_dir(&partial);
        // A version already kept, the current one on a reinstall, is set
        // aside and comes back if the install fails.
        let version_dir = self.version_dir(&name, &version)?;
        let replaced = app_dir.join(format!(".{version}.replaced"));
        let _ = std::fs::remove_dir_all(&replaced);
        let replacing = version_dir.exists();
        if replacing {
            std::fs::rename(&version_dir, &replaced)
                .with_context(|| format!("set {} aside", version_dir.display()))?;
        }
        std::fs::rename(&partial, &version_dir)
            .with_context(|| format!("move the bundle to {}", version_dir.display()))?;

        let data = self.data_dir(&name)?;
        let data_was_there = data.exists();
        make_data_dir(&data)?;
        let from = match &old {
            Some(entry) if replacing && entry.current.version == version => {
                bundle::read_manifest(&replaced).ok()
            }
            Some(entry) => self.manifest_of(&name, &entry.current.version).ok(),
            None => None,
        };
        let run_hook = manifest.first_run.is_some()
            && !old
                .as_ref()
                .is_some_and(|entry| entry.first_run.contains(&version));
        let switched = self.switch(&name, from.as_ref(), &manifest, run_hook).await;
        let action = if old.is_some() { "upgrade" } else { "install" };
        match switched {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&replaced);
                let installed = Installed {
                    version: version.clone(),
                    sha256: unpacked.sha256.clone(),
                    source: source.to_string(),
                    installed_at: now(),
                };
                let entry = record.apps.entry(name.clone()).or_insert_with(|| AppEntry {
                    current: installed.clone(),
                    previous: None,
                    first_run: Default::default(),
                    last: None,
                    update: Default::default(),
                });
                if entry.current.version != version {
                    entry.previous = Some(std::mem::replace(&mut entry.current, installed));
                } else {
                    entry.current = installed;
                }
                if run_hook {
                    entry.first_run.insert(version.clone());
                }
                entry.last = Some(last(action, "ok", None));
                self.prune(&name, entry);
                self.write_record(&record)?;
                tracing::info!(app = name, version, source, action, "app installed");
                Ok(json!({ "name": name, "version": version, "action": action }))
            }
            Err(err) => {
                let err = err.context(format!("{action} {name} {version}"));
                let _ = std::fs::remove_dir_all(&version_dir);
                if replacing {
                    let _ = std::fs::rename(&replaced, &version_dir);
                }
                match (&old, &from) {
                    (Some(entry), Some(from)) => {
                        let back = self.switch(&name, Some(&manifest), from, false).await;
                        let message = match back {
                            Ok(()) => format!("{err:#}; rolled back to {}", entry.current.version),
                            Err(back) => {
                                format!("{err:#}; the roll back failed too: {back:#}")
                            }
                        };
                        record_last(&mut record, &name, last(action, "failed", Some(&message)));
                        self.write_record(&record)?;
                        Err(AppError::Failed(anyhow::anyhow!(message)))
                    }
                    (Some(_), None) => {
                        let message =
                            format!("{err:#}; the installed version's manifest is unreadable");
                        record_last(&mut record, &name, last(action, "failed", Some(&message)));
                        self.write_record(&record)?;
                        Err(AppError::Failed(anyhow::anyhow!(message)))
                    }
                    (None, _) => {
                        self.teardown(&manifest).await;
                        let _ = std::fs::remove_dir_all(&app_dir);
                        if !data_was_there {
                            let _ = std::fs::remove_dir_all(&data);
                        }
                        Err(AppError::Failed(err))
                    }
                }
            }
        }
    }

    /// Switch `name` from `from` to `to`, whose version directory is in place:
    /// stop the old units, install the new ones and remove the gone, point
    /// `current` and `app.env` at it, run its first-run hook if `run_hook`,
    /// then enable and start its `enable` units.
    async fn switch(
        &self,
        name: &str,
        from: Option<&Manifest>,
        to: &Manifest,
        run_hook: bool,
    ) -> anyhow::Result<()> {
        let init = self.init();
        let control = self.units.control();
        let old = from
            .map(|m| manifest::units_for(m, init))
            .unwrap_or_default();
        let new = manifest::units_for(to, init);
        for (unit, _) in &old {
            control
                .stop(&self.units.control_name(unit))
                .await
                .with_context(|| format!("stop {unit}"))?;
        }
        let dir = self.version_dir(name, &to.version).map_err(anyhow_of)?;
        let units = dir.join("units").join(init.as_str());
        for (unit, _) in &new {
            let text = std::fs::read_to_string(units.join(unit))
                .with_context(|| format!("read {unit}"))?;
            self.units.install(unit, &text, false).await?;
            if init == Init::Openrc
                && let Ok(conf) = std::fs::read_to_string(units.join("conf.d").join(unit))
            {
                self.units
                    .install(&format!("{unit}.conf"), &conf, true)
                    .await?;
            }
        }
        for (unit, _) in &old {
            if !new.iter().any(|(kept, _)| kept == unit) {
                self.units.mica_unit(&["remove", unit]).await?;
            }
        }
        self.point_current(name, &to.version)?;
        self.write_env(name, to)?;
        if run_hook && let Some(hook) = &to.first_run {
            self.run_hook(name, &dir, hook).await?;
        }
        let listed = self.units.listed().await?;
        for (unit, enable) in &new {
            if listed.get(unit).copied().unwrap_or(false) {
                self.units.mica_unit(&["disable", unit]).await?;
            }
            if *enable {
                self.units.mica_unit(&["enable", unit]).await?;
                control
                    .start(&self.units.control_name(unit))
                    .await
                    .with_context(|| format!("start {unit}"))?;
            }
        }
        for (unit, enable) in &new {
            if *enable
                && control.active_state(&self.units.control_name(unit)).await? == FAILED_STATE
            {
                bail!("{unit} failed to start");
            }
        }
        Ok(())
    }

    /// Stop and remove `manifest`'s units, whatever state they are in.
    async fn teardown(&self, manifest: &Manifest) {
        let listed = self.units.listed().await.unwrap_or_default();
        for (unit, _) in manifest::units_for(manifest, self.init()) {
            if let Err(err) = self
                .units
                .control()
                .stop(&self.units.control_name(&unit))
                .await
            {
                tracing::warn!(unit, %err, "stop an app unit");
            }
            if listed.contains_key(&unit)
                && let Err(err) = self.units.mica_unit(&["remove", &unit]).await
            {
                tracing::warn!(unit, %err, "remove an app unit");
            }
        }
    }

    /// Point `current` at `version`, in one rename.
    fn point_current(&self, name: &str, version: &str) -> anyhow::Result<()> {
        let app_dir = self.app_dir(name).map_err(anyhow_of)?;
        let next = app_dir.join(".current.next");
        let _ = std::fs::remove_file(&next);
        std::os::unix::fs::symlink(version, &next)
            .with_context(|| format!("link {}", next.display()))?;
        std::fs::rename(&next, app_dir.join("current")).context("switch current")?;
        Ok(())
    }

    /// `app.env`: what the app's units and hook read.
    fn write_env(&self, name: &str, manifest: &Manifest) -> anyhow::Result<()> {
        let mut text = String::new();
        for (key, value) in self.env_of(name, manifest)? {
            text.push_str(&format!("{key}=\"{value}\"\n"));
        }
        let path = self.app_dir(name).map_err(anyhow_of)?.join(ENV_FILE);
        crate::fswrite::write_config(&path, &text, 0o644)
    }

    fn env_of(&self, name: &str, manifest: &Manifest) -> anyhow::Result<Vec<(String, String)>> {
        let device = |path: &Path| {
            let root = self.root.as_deref().unwrap_or(Path::new("/"));
            Path::new("/")
                .join(path.strip_prefix(root).unwrap_or(path))
                .display()
                .to_string()
        };
        let mut env = vec![
            ("APP_NAME".to_string(), name.to_string()),
            ("APP_VERSION".to_string(), manifest.version.clone()),
            (
                "APP_DIR".to_string(),
                device(&self.app_dir(name).map_err(anyhow_of)?.join("current")),
            ),
            (
                "APP_DATA".to_string(),
                device(&self.data_dir(name).map_err(anyhow_of)?),
            ),
        ];
        env.extend(manifest.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        Ok(env)
    }

    /// Run the first-run hook as root in `dir`, bounded; its output is kept in
    /// the app's directory and sent to the system log.
    async fn run_hook(&self, name: &str, dir: &Path, hook: &str) -> anyhow::Result<()> {
        let manifest = bundle::read_manifest(dir).map_err(|err| anyhow::anyhow!("{err}"))?;
        let run = tokio::process::Command::new(dir.join(hook))
            .current_dir(dir)
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .envs(self.env_of(name, &manifest)?)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(self.hook_timeout, run).await;
        let (log, outcome) = match output {
            Err(_) => (
                Vec::new(),
                Err(anyhow::anyhow!(
                    "the first-run hook did not finish within {:?}",
                    self.hook_timeout
                )),
            ),
            Ok(Err(err)) => (
                Vec::new(),
                Err(anyhow::Error::new(err).context("run the first-run hook")),
            ),
            Ok(Ok(output)) => {
                let mut log = output.stdout;
                log.extend(output.stderr);
                let outcome = if output.status.success() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "the first-run hook exited {}",
                        output.status
                    ))
                };
                (log, outcome)
            }
        };
        let log = &log[log.len().saturating_sub(HOOK_LOG_BYTES)..];
        let path = self.app_dir(name).map_err(anyhow_of)?.join(HOOK_LOG);
        if let Err(err) = std::fs::write(&path, log) {
            tracing::warn!(path = %path.display(), %err, "keep the first-run output");
        }
        if let Some(logger) = &self.logger {
            send_to_log(logger, &format!("mica-app-{name}"), log).await;
        }
        outcome
    }

    /// Keep only `entry`'s current and previous versions.
    fn prune(&self, name: &str, entry: &AppEntry) {
        let Ok(app_dir) = self.app_dir(name) else {
            return;
        };
        let keep: Vec<&str> = std::iter::once(entry.current.version.as_str())
            .chain(entry.previous.as_ref().map(|p| p.version.as_str()))
            .collect();
        let Ok(dir) = std::fs::read_dir(&app_dir) else {
            return;
        };
        for item in dir.flatten() {
            let file_name = item.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let is_version =
                item.file_type().is_ok_and(|t| t.is_dir()) && !file_name.starts_with('.');
            if is_version && !keep.contains(&file_name) {
                let _ = std::fs::remove_dir_all(item.path());
            }
        }
    }

    /// Switch `name` back to its previous version; its hook does not run.
    ///
    /// # Errors
    ///
    /// No such app, no previous version, or the switch failed (and was
    /// undone).
    pub async fn rollback(&self, name: &str) -> Result<Value, AppError> {
        let mut record = self.read_record();
        let entry = record
            .apps
            .get(name)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        let previous = entry.previous.clone().ok_or_else(|| {
            AppError::Refused(format!("{name} has no previous version to roll back to"))
        })?;
        let current = self.manifest_of(name, &entry.current.version)?;
        let target = self.manifest_of(name, &previous.version)?;
        if let Err(err) = self.switch(name, Some(&current), &target, false).await {
            let back = self.switch(name, Some(&target), &current, false).await;
            let mut message = format!("roll back {name} to {}: {err:#}", previous.version);
            if let Err(back) = back {
                message.push_str(&format!(
                    "; the return to {} failed too: {back:#}",
                    current.version
                ));
            }
            record_last(
                &mut record,
                name,
                last("rollback", "failed", Some(&message)),
            );
            self.write_record(&record)?;
            return Err(AppError::Failed(anyhow::anyhow!(message)));
        }
        if let Some(entry) = record.apps.get_mut(name) {
            entry.previous = Some(std::mem::replace(&mut entry.current, previous.clone()));
            entry.last = Some(last("rollback", "ok", None));
        }
        self.write_record(&record)?;
        tracing::info!(app = name, version = previous.version, "app rolled back");
        Ok(json!({ "name": name, "version": previous.version, "action": "rollback" }))
    }

    /// Stop and remove `name`'s units and its versions; `purge` deletes its
    /// data too.
    ///
    /// # Errors
    ///
    /// No such app, or its files could not be removed.
    pub async fn remove(&self, name: &str, purge: bool) -> Result<Value, AppError> {
        let mut record = self.read_record();
        let entry = record
            .apps
            .get(name)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        if let Ok(manifest) = self.manifest_of(name, &entry.current.version) {
            self.teardown(&manifest).await;
        }
        let app_dir = self.app_dir(name)?;
        match std::fs::remove_dir_all(&app_dir) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(anyhow::Error::new(err)
                    .context(format!("remove {}", app_dir.display()))
                    .into());
            }
            _ => {}
        }
        if purge {
            let data = self.data_dir(name)?;
            match std::fs::remove_dir_all(&data) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return Err(anyhow::Error::new(err)
                        .context(format!("remove {}", data.display()))
                        .into());
                }
                _ => {}
            }
        }
        record.apps.remove(name);
        self.write_record(&record)?;
        tracing::info!(app = name, purge, "app removed");
        Ok(json!({ "name": name, "action": "remove", "purged": purge }))
    }

    /// Start, stop or restart every unit of `name`.
    ///
    /// # Errors
    ///
    /// No such app, or the init refused.
    pub async fn act(&self, name: &str, verb: Verb) -> Result<(), AppError> {
        let entry = self
            .read_record()
            .apps
            .remove(name)
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        let manifest = self.manifest_of(name, &entry.current.version)?;
        for (unit, _) in manifest::units_for(&manifest, self.init()) {
            self.units
                .act(&unit, verb)
                .await
                .with_context(|| format!("{verb:?} {unit}"))?;
        }
        Ok(())
    }

    /// `name`'s units' newest log lines and its last first-run output.
    ///
    /// # Errors
    ///
    /// No such app.
    pub async fn logs(
        &self,
        name: &str,
        reader: &dyn crate::logs::LogReader,
    ) -> Result<Value, AppError> {
        let entry = self
            .read_record()
            .apps
            .remove(name)
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        let manifest = self.manifest_of(name, &entry.current.version)?;
        let mut units = Vec::new();
        for (unit, _) in manifest::units_for(&manifest, self.init()) {
            let source = self.units.log_source(&unit);
            let mut log = crate::logs::log_json(reader, &source).await;
            log["unit"] = json!(unit);
            units.push(log);
        }
        let first_run = std::fs::read(self.app_dir(name)?.join(HOOK_LOG))
            .ok()
            .map(|log| {
                String::from_utf8_lossy(&log)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
        Ok(json!({ "name": name, "units": units, "firstRun": first_run }))
    }
}

fn anyhow_of(err: AppError) -> anyhow::Error {
    match err {
        AppError::Failed(err) => err,
        other => anyhow::anyhow!("{other}"),
    }
}

fn last(action: &str, outcome: &str, message: Option<&str>) -> LastResult {
    LastResult {
        action: action.to_string(),
        outcome: outcome.to_string(),
        message: message.map(str::to_string),
        at: now(),
    }
}

fn record_last(record: &mut Record, name: &str, result: LastResult) {
    if let Some(entry) = record.apps.get_mut(name) {
        entry.last = Some(result);
    }
}

/// `/srv/apps/<name>`, the app's own: root's, 0700.
fn make_data_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(err).with_context(|| format!("create {}", path.display()));
        }
        _ => {}
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod {}", path.display()))
}

/// Refuse a bundle the filesystem `dest` is on has no room to unpack: what it
/// unpacks to, read from its headers, must fit with the versions kept.
fn ensure_room(archive: &Path, dest: &Path) -> Result<(), bundle::Refusal> {
    let needed = bundle::unpacked_size(archive)?;
    let dir = dest.parent().unwrap_or(dest);
    let free = rustix::fs::statvfs(dir)
        .map(|stat| stat.f_bavail.saturating_mul(stat.f_frsize))
        .map_err(|err| {
            bundle::Refusal::Io(anyhow::Error::new(err).context("read the free space"))
        })?;
    if needed > free {
        return Err(Problem::new(
            "too_large",
            format!(
                "the bundle unpacks to {} MiB and {} MiB is free",
                needed >> 20,
                free >> 20
            ),
        )
        .into());
    }
    Ok(())
}

/// Flush what was unpacked under `dir` before it is renamed into place.
fn sync_dir(dir: &Path) {
    if let Ok(file) = std::fs::File::open(dir)
        && let Err(err) = rustix::fs::syncfs(&file)
    {
        tracing::warn!(dir = %dir.display(), %err, "sync the unpacked bundle");
    }
}

/// Send `log` to the system log under `tag`, through `logger`.
async fn send_to_log(logger: &str, tag: &str, log: &[u8]) {
    use tokio::io::AsyncWriteExt;
    if log.is_empty() {
        return;
    }
    let child = tokio::process::Command::new(logger)
        .args(["-t", tag])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(log).await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await;
}
