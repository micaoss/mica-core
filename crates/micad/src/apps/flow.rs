//! Install, upgrade, roll back and remove an app.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, bail};
use micad_settings::{Init, ServicesSettings};
use serde_json::{Value, json};

use super::bundle;
use super::manifest::{self, Manifest, Privileges, Problem, RunAs};
use super::render::{self, Account};
use super::{
    AppEntry, AppError, Apps, Installed, LastResult, MAX_APPS, Record, UID_COUNT, account_name, now,
};
use crate::user_units::Verb;

/// The most bytes of a first-run hook's output kept.
const HOOK_LOG_BYTES: usize = 64 * 1024;
/// The first-run hook's output, in the app's directory.
const HOOK_LOG: &str = "first-run.log";
/// The environment file, in the app's directory.
const ENV_FILE: &str = "app.env";
/// The `ActiveState` of a unit that did not start.
const FAILED_STATE: &str = "failed";

/// The groups a device node may not have for an app's own user to be given it:
/// each of these reaches every disk, the memory, the terminals or the
/// accounts.
const DENIED_GROUPS: [&str; 8] = [
    "disk", "kmem", "shadow", "sudo", "wheel", "adm", "tty", "utmp",
];

/// What an install asks for besides the bundle.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Install the version that is installed again.
    pub reinstall: bool,
    /// Refuse a bundle of an app that is not installed.
    pub upgrade: bool,
    /// Refuse a bundle that asks for more privileges than the installed
    /// version: an update no one started.
    pub unattended: bool,
    /// Answer [`AppError::Privileges`] for a bundle that asks for more
    /// privileges than the installed version, or for any on a first install:
    /// the one who asked has not seen them yet.
    pub ask_privileges: bool,
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
            problems.extend(self.device_problems(&manifest)?);
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
        if options.unattended || options.ask_privileges {
            let entry = record.apps.get(&manifest.name);
            let installed = match entry {
                Some(entry) => Some(self.manifest_of(&manifest.name, &entry.current.version)?),
                None => None,
            };
            let delta = privilege_delta(
                installed.as_ref().map(|installed| &installed.privileges),
                &manifest.privileges,
            );
            match (delta, entry) {
                (Some(_), Some(entry)) if options.unattended => {
                    return Err(AppError::Refused(format!(
                        "{} {} asks for more privileges than {}; only an install by hand takes it",
                        manifest.name, manifest.version, entry.current.version
                    )));
                }
                (Some(delta), _) if options.ask_privileges => {
                    return Err(AppError::Privileges(
                        format!(
                            "{} {} asks for privileges{}; accept them to install it",
                            manifest.name,
                            manifest.version,
                            entry.map_or(String::new(), |entry| format!(
                                " beyond {}",
                                entry.current.version
                            ))
                        ),
                        delta,
                    ));
                }
                _ => {}
            }
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
        let had_uid = record.uids.contains_key(&name);
        let account = self.account(&name, &manifest, &mut record)?;
        let switched = match &account {
            Some(account) => self.name_account(&name, account.uid).await,
            None => Ok(()),
        };
        let switched = match switched {
            Ok(()) => {
                self.switch(&name, from.as_ref(), &manifest, account.as_ref(), run_hook)
                    .await
            }
            Err(err) => Err(err),
        };
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
                        let back = match self.account(&name, from, &mut record) {
                            Ok(back_account) => {
                                self.switch(
                                    &name,
                                    Some(&manifest),
                                    from,
                                    back_account.as_ref(),
                                    false,
                                )
                                .await
                            }
                            Err(err) => Err(anyhow_of(err)),
                        };
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
                        if account.is_some() && !had_uid {
                            let _ = self.accounts_tool(&["remove", &account_name(&name)]).await;
                        }
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
    /// `current` and `app.env` at it, give its data to `account` (root's
    /// without one), run its first-run hook if `run_hook`, then enable and
    /// start its `enable` units. A service's unit is written from the
    /// manifest; a root app's comes from its bundle.
    async fn switch(
        &self,
        name: &str,
        from: Option<&Manifest>,
        to: &Manifest,
        account: Option<&Account>,
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
        let vars: BTreeMap<String, String> = self.env_of(name, to)?.into_iter().collect();
        for (unit, _) in &new {
            let service = to
                .services
                .iter()
                .find(|service| manifest::service_unit(&service.name, init) == *unit);
            let text = match service {
                Some(service) => render::unit(
                    init,
                    name,
                    service,
                    account.context("a service runs as the app's own user")?,
                    &to.privileges.devices,
                    &vars,
                )?,
                None => std::fs::read_to_string(units.join(unit))
                    .with_context(|| format!("read {unit}"))?,
            };
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
        self.own_data(name, account.map(|account| account.uid))?;
        if run_hook && let Some(hook) = &to.first_run {
            self.run_hook(name, &dir, hook, account).await?;
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

    /// The devices of `manifest` an app's own user may not be given: a node
    /// that is not a character device, or one whose group reaches past it,
    /// since that group becomes the app's.
    fn device_problems(&self, manifest: &Manifest) -> Result<Vec<Problem>, AppError> {
        let root = self.root()?;
        let mut problems = Vec::new();
        for device in &manifest.privileges.devices {
            let Ok(meta) = std::fs::metadata(root.join(device.trim_start_matches('/'))) else {
                continue;
            };
            if let Some(why) = self.device_refusal(&meta) {
                problems.push(Problem::new(
                    "device",
                    format!("{device} {why}; an app that needs it runs as root"),
                ));
            }
        }
        Ok(problems)
    }

    /// Why a node of `meta` may not be an app's own user's, if it may not.
    fn device_refusal(&self, meta: &std::fs::Metadata) -> Option<String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if !meta.file_type().is_char_device() {
            return Some("is not a character device".into());
        }
        let groups = self
            .root()
            .ok()
            .and_then(|root| std::fs::read_to_string(root.join("etc/group")).ok());
        group_refusal(meta.gid(), groups.as_deref())
    }

    /// Who `manifest`'s processes run as: nobody for a root app, else the
    /// app's own uid, kept in `record` or given there now.
    fn account(
        &self,
        name: &str,
        manifest: &Manifest,
        record: &mut Record,
    ) -> Result<Option<Account>, AppError> {
        use std::os::unix::fs::MetadataExt;
        if manifest.privileges.user == RunAs::Root {
            return Ok(None);
        }
        let uid = match record.uids.get(name) {
            Some(uid) => *uid,
            None => {
                let taken = self.taken_ids(record);
                let uid = (self.uid_base..self.uid_base.saturating_add(UID_COUNT))
                    .find(|id| !taken.contains(id))
                    .ok_or_else(|| AppError::Refused("every uid for apps is taken".into()))?;
                record.uids.insert(name.to_string(), uid);
                uid
            }
        };
        let root = self.root()?;
        let mut groups: Vec<u32> = manifest
            .privileges
            .devices
            .iter()
            .filter_map(|device| std::fs::metadata(root.join(device.trim_start_matches('/'))).ok())
            .filter(|meta| self.device_refusal(meta).is_none())
            .map(|meta| meta.gid())
            .filter(|gid| *gid != 0)
            .collect();
        groups.sort_unstable();
        groups.dedup();
        Ok(Some(Account {
            uid,
            groups,
            capabilities: manifest.privileges.capabilities.clone(),
        }))
    }

    /// Name `uid` `mica-app-<name>` through mica-accounts, which keeps it
    /// unchanged when it has it already.
    async fn name_account(&self, name: &str, uid: u32) -> anyhow::Result<()> {
        let uid = uid.to_string();
        self.accounts_tool(&["add", &account_name(name), &uid, &uid])
            .await
    }

    /// Run mica-accounts with `args`; nothing on a root without it, whose
    /// apps' uids stay unnamed.
    async fn accounts_tool(&self, args: &[&str]) -> anyhow::Result<()> {
        if !self.accounts.exists() {
            return Ok(());
        }
        let output = tokio::process::Command::new(&self.accounts)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .with_context(|| format!("run {}", self.accounts.display()))?;
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    /// The ids the root's accounts and groups and the apps hold.
    fn taken_ids(&self, record: &Record) -> BTreeSet<u32> {
        let mut taken: BTreeSet<u32> = record.uids.values().copied().collect();
        if let Ok(root) = self.root() {
            for file in ["etc/passwd", "etc/group"] {
                if let Ok(text) = std::fs::read_to_string(root.join(file)) {
                    taken.extend(
                        text.lines()
                            .filter_map(|line| line.split(':').nth(2)?.parse::<u32>().ok()),
                    );
                }
            }
        }
        taken
    }

    /// Give `name`'s data to `uid`, or to micad's own user (root on a
    /// device), unless it is theirs already.
    fn own_data(&self, name: &str, uid: Option<u32>) -> anyhow::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let data = self.data_dir(name).map_err(anyhow_of)?;
        let owner = uid.unwrap_or_else(|| rustix::process::geteuid().as_raw());
        let meta =
            std::fs::symlink_metadata(&data).with_context(|| format!("read {}", data.display()))?;
        if meta.uid() == owner && meta.gid() == owner {
            return Ok(());
        }
        chown_tree(&data, owner)
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

    /// Run the first-run hook in `dir`, bounded: as `account`, with no
    /// capabilities, or as root without one. Its output is kept in the app's
    /// directory and sent to the system log.
    async fn run_hook(
        &self,
        name: &str,
        dir: &Path,
        hook: &str,
        account: Option<&Account>,
    ) -> anyhow::Result<()> {
        let manifest = bundle::read_manifest(dir).map_err(|err| anyhow::anyhow!("{err}"))?;
        let vars = self.env_of(name, &manifest)?;
        // setpriv runs as root, so it gets PATH alone; `env -i` sets the
        // app's variables once it runs as the app.
        let mut command = match account {
            Some(account) => {
                let mut command = tokio::process::Command::new(&self.setpriv);
                command
                    .args(render::setpriv_args(&Account {
                        capabilities: Vec::new(),
                        ..account.clone()
                    }))
                    .args(render::env_args(
                        dir.join(hook).to_string_lossy().into_owned(),
                        &vars,
                    )?);
                command
            }
            None => tokio::process::Command::new(dir.join(hook)),
        };
        command
            .current_dir(dir)
            .env_clear()
            .env("PATH", render::PATH);
        if account.is_none() {
            command.envs(vars);
        }
        let run = command
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
            send_to_log(logger, &account_name(name), log).await;
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
        let current_account = self.account(name, &current, &mut record)?;
        let target_account = self.account(name, &target, &mut record)?;
        if let Err(err) = self
            .switch(
                name,
                Some(&current),
                &target,
                target_account.as_ref(),
                false,
            )
            .await
        {
            let back = self
                .switch(
                    name,
                    Some(&target),
                    &current,
                    current_account.as_ref(),
                    false,
                )
                .await;
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
        if purge && record.uids.remove(name).is_some() {
            self.accounts_tool(&["remove", &account_name(name)])
                .await
                .unwrap_or_else(|err| tracing::warn!(app = name, %err, "remove the app's account"));
        }
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

/// `/srv/apps/<name>`, 0700; [`Apps::own_data`] gives it to the app's user.
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

/// `path` and everything under it, links not followed, to `id`:`id`.
fn chown_tree(path: &Path, id: u32) -> anyhow::Result<()> {
    std::os::unix::fs::lchown(path, Some(id), Some(id))
        .with_context(|| format!("chown {}", path.display()))?;
    if std::fs::symlink_metadata(path)?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), id)?;
        }
    }
    Ok(())
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

/// Why a node of group `gid` may not be an app's own user's, given the root's
/// `/etc/group`: its group is one of [`DENIED_GROUPS`], or has no name there.
/// Group 0 is never given, so it is never refused.
pub(super) fn group_refusal(gid: u32, groups: Option<&str>) -> Option<String> {
    if gid == 0 {
        return None;
    }
    let id = gid.to_string();
    let name = groups.and_then(|text| {
        text.lines().find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            (fields.nth(1)? == id).then_some(name)
        })
    });
    match name {
        None => Some(format!(
            "belongs to gid {gid}, which has no name in /etc/group"
        )),
        Some(name) if DENIED_GROUPS.contains(&name) => Some(format!(
            "belongs to group {name}, which reaches past the app"
        )),
        Some(_) => None,
    }
}

/// What an app asks for beyond what it has.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PrivilegeDelta {
    /// It runs as root, with full control of the device.
    pub root: bool,
    /// `CAP_*` names it keeps.
    pub capabilities: Vec<String>,
    /// Device nodes it opens.
    pub devices: Vec<String>,
}

/// What `next` asks for beyond `installed` (nothing on a first install):
/// root where `installed` did not run as root, or capabilities and devices
/// it did not have. None when it asks for nothing more.
pub(super) fn privilege_delta(
    installed: Option<&Privileges>,
    next: &Privileges,
) -> Option<PrivilegeDelta> {
    if installed.is_some_and(|installed| installed.user == RunAs::Root) {
        return None;
    }
    let delta = if next.user == RunAs::Root {
        PrivilegeDelta {
            root: true,
            ..PrivilegeDelta::default()
        }
    } else {
        let new = |had: Option<&Vec<String>>, now: &[String]| -> Vec<String> {
            now.iter()
                .filter(|item| !had.is_some_and(|had| had.contains(item)))
                .cloned()
                .collect()
        };
        PrivilegeDelta {
            root: false,
            capabilities: new(
                installed.map(|installed| &installed.capabilities),
                &next.capabilities,
            ),
            devices: new(installed.map(|installed| &installed.devices), &next.devices),
        }
    };
    (delta != PrivilegeDelta::default()).then_some(delta)
}
