//! Logging reconciler: the `logging` settings subtree rendered into Base's
//! `/var/lib/mica/log/log.env`.
//!
//! Base owns everything after the file. Its `mica-log-env` layers the image's
//! `/usr/lib/mica/log/log.env`, a package's `log.d/*.env` (raw NAND keeps the
//! log in RAM) and this file, later keys winning, and both inits apply what it
//! prints: `mica-log-settings.service` renders journald's cap and
//! systemd-netlogd's collector, `mica-syslogd` its arguments. A value the
//! settings leave unset is left out of the file, so the image's and the
//! board's defaults decide it without micad knowing them.
//!
//! A file Base refuses puts the log in RAM and forwards nothing, so a render is
//! held to `mica-log-env` before it replaces the live file: a refusal fails the
//! apply with Base's reason and leaves the live file as it was. What the live
//! state reports is what `mica-log-env` prints, the values in force.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use micad_settings::{LoggingSettings, Settings};
use serde_json::json;

use super::Reconciler;
use super::systemd::{Systemd, UnitControl};
use crate::fswrite::write_config;
use crate::openrc::{Commands, Host, OpenrcUnits};

/// What applies the settings on a systemd root: journald's cap and the
/// collector of systemd-netlogd, rendered from `mica-log-env`.
const SYSTEMD_UNIT: &str = "mica-log-settings.service";
/// busybox syslogd on an OpenRC root, whose arguments `mica-log-env` decides.
const OPENRC_UNIT: &str = "mica-syslogd.service";
/// The file micad renders, the last one `mica-log-env` reads.
const DEFAULT_SETTINGS_PATH: &str = "/var/lib/mica/log/log.env";
/// Base's reader of the log settings.
const DEFAULT_CHECKER: &str = "/usr/lib/mica/mica-log-env";
/// The render's sibling while Base checks it.
const STAGED_NAME: &str = ".log.env.micad-staged";
/// Mode of the render: it carries no secret.
const SETTINGS_MODE: u32 = 0o644;

/// Reconciler for the `logging` settings subtree.
pub struct LoggingReconciler<C: UnitControl> {
    /// The unit that applies the settings.
    unit: &'static str,
    /// Where the settings are rendered.
    settings_path: PathBuf,
    /// Base's `mica-log-env`.
    checker: PathBuf,
    commands: Arc<dyn Commands>,
    control: C,
}

impl<C: UnitControl> LoggingReconciler<C> {
    pub fn new(
        unit: &'static str,
        settings_path: PathBuf,
        checker: PathBuf,
        commands: Arc<dyn Commands>,
        control: C,
    ) -> Self {
        Self {
            unit,
            settings_path,
            checker,
            commands,
            control,
        }
    }
}

impl LoggingReconciler<Systemd> {
    /// Production reconciler on a systemd root.
    pub fn production() -> Self {
        Self::new(
            SYSTEMD_UNIT,
            PathBuf::from(DEFAULT_SETTINGS_PATH),
            PathBuf::from(DEFAULT_CHECKER),
            Arc::new(Host),
            Systemd::new(),
        )
    }
}

impl LoggingReconciler<OpenrcUnits> {
    /// Production reconciler on an OpenRC root.
    pub fn openrc() -> Self {
        Self::new(
            OPENRC_UNIT,
            PathBuf::from(DEFAULT_SETTINGS_PATH),
            PathBuf::from(DEFAULT_CHECKER),
            Arc::new(Host),
            OpenrcUnits::production(),
        )
    }
}

/// `log.env` for `logging`: only the keys the settings set, so an unset one
/// keeps the image's or the board's value. Forwarding switched off writes an
/// empty collector, which overrides any default one.
fn render(logging: &LoggingSettings) -> String {
    let mut out = String::from(
        "# The log settings micad renders from `logging`; Base's mica-log-env reads them\n\
         # after the image's and the packages' defaults.\n",
    );
    if let Some(persistent) = logging.persistent {
        out.push_str(&format!(
            "LOG_PERSISTENT={}\n",
            if persistent { "yes" } else { "no" }
        ));
    }
    if let Some(cap) = logging.max_size_kib {
        out.push_str(&format!("LOG_MAX_KIB={cap}\n"));
    }
    let remote = &logging.remote;
    let collector = match remote.address.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) if remote.enabled => format!("{address}:{}", remote.port),
        Ok(IpAddr::V6(address)) if remote.enabled => format!("[{address}]:{}", remote.port),
        _ => String::new(),
    };
    out.push_str(&format!("LOG_REMOTE={collector}\n"));
    out.push_str(&format!(
        "LOG_REMOTE_PROTOCOL={}\n",
        remote.protocol.as_str()
    ));
    out
}

/// What `mica-log-env` said of one settings file.
struct Verdict {
    /// Its reason, when it refused the file.
    refusal: Option<String>,
    /// The values in force: the file's, or on a refusal the safe ones.
    values: BTreeMap<String, String>,
}

impl<C: UnitControl> LoggingReconciler<C> {
    /// `mica-log-env` over `settings` in place of the live file.
    async fn check(&self, settings: &Path) -> Result<Verdict> {
        let variable = format!("MICA_LOG_SETTINGS={}", settings.display());
        let checker = self.checker.display().to_string();
        let output = self.commands.run("env", &[&variable, &checker]).await?;
        let values = output
            .stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let refusal = match output.code {
            0 => None,
            1 => Some(output.stderr.trim().to_string()),
            code => bail!("{checker} exited {code}: {}", output.stderr.trim()),
        };
        Ok(Verdict { refusal, values })
    }

    /// Replace the live file with `rendered` once Base takes it; reports
    /// whether the bytes changed.
    async fn render_checked(&self, rendered: &str) -> Result<bool> {
        if std::fs::read_to_string(&self.settings_path).is_ok_and(|live| live == rendered) {
            return Ok(false);
        }
        let directory = self
            .settings_path
            .parent()
            .context("the log settings path has no directory")?;
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let staged = directory.join(STAGED_NAME);
        write_config(&staged, rendered, SETTINGS_MODE)
            .with_context(|| format!("render {}", staged.display()))?;
        let verdict = self.check(&staged).await;
        if let Some(reason) = verdict.as_ref().ok().and_then(|v| v.refusal.clone()) {
            let _ = std::fs::remove_file(&staged);
            bail!("Base refuses these log settings, so they are not applied: {reason}");
        }
        if let Err(error) = verdict {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        std::fs::rename(&staged, &self.settings_path)
            .with_context(|| format!("replace {}", self.settings_path.display()))?;
        std::fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync {}", directory.display()))?;
        Ok(true)
    }
}

/// `LOG_REMOTE` in force, as the live state reports it.
fn remote_json(values: &BTreeMap<String, String>) -> serde_json::Value {
    let protocol = values
        .get("LOG_REMOTE_PROTOCOL")
        .cloned()
        .unwrap_or_default();
    let collector = values.get("LOG_REMOTE").map_or("", String::as_str);
    let parsed = collector.rsplit_once(':').and_then(|(address, port)| {
        let address = address.trim_start_matches('[').trim_end_matches(']');
        Some((address.parse::<IpAddr>().ok()?, port.parse::<u16>().ok()?))
    });
    match parsed {
        Some((address, port)) => json!({
            "enabled": true,
            "address": address.to_string(),
            "port": port,
            "protocol": protocol,
        }),
        None => json!({ "enabled": false, "protocol": protocol }),
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for LoggingReconciler<C> {
    fn name(&self) -> &'static str {
        "logging"
    }

    fn subtree(&self) -> &'static str {
        "logging"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        if !self.checker.exists() {
            return Ok(json!({
                "supported": false,
                "detail": format!(
                    "this root has no {}, so the log settings are not applied",
                    self.checker.display()
                ),
            }));
        }
        let logging = &settings.logging;
        if self.render_checked(&render(logging)).await? {
            self.control.restart(self.unit).await?;
        }
        let live = self.check(&self.settings_path).await?;
        let number = |key: &str| {
            live.values
                .get(key)
                .and_then(|value| value.parse::<u64>().ok())
        };
        let source = |set: bool| if set { "setting" } else { "default" };
        let mut state = json!({
            "supported": true,
            "valid": live.refusal.is_none(),
            "persistent": live.values.get("LOG_PERSISTENT").map(|value| value == "yes"),
            "persistentSource": source(logging.persistent.is_some()),
            "maxSizeKiB": number("LOG_MAX_KIB"),
            "maxSizeKiBSource": source(logging.max_size_kib.is_some()),
            "maxSizeKiBLimit": number("LOG_MAX_KIB_LIMIT"),
            "remote": remote_json(&live.values),
            "settingsPath": self.settings_path.display().to_string(),
            "unit": self.unit,
        });
        if let Some(reason) = live.refusal {
            state["detail"] = json!(reason);
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use micad_settings::{RemoteLogProtocol, RemoteLogSettings};

    use super::super::systemd::mock::MockUnitControl;
    use super::*;
    use crate::openrc::{FakeCommands, Output};

    /// What a device with nothing set renders: no persistence and no cap,
    /// so the image's and the board's values stand, and no collector.
    const GOLDEN_UNSET: &str = "# The log settings micad renders from `logging`; Base's mica-log-env reads them\n\
         # after the image's and the packages' defaults.\n\
         LOG_REMOTE=\n\
         LOG_REMOTE_PROTOCOL=udp\n";

    fn logging(
        persistent: Option<bool>,
        cap: Option<u32>,
        address: &str,
        protocol: RemoteLogProtocol,
    ) -> Settings {
        Settings {
            logging: LoggingSettings {
                persistent,
                max_size_kib: cap,
                remote: RemoteLogSettings {
                    enabled: !address.is_empty(),
                    address: address.into(),
                    port: 514,
                    protocol,
                },
            },
            ..Settings::default()
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        live: PathBuf,
        staged: PathBuf,
        checker: String,
        commands: Arc<FakeCommands>,
        reconciler: LoggingReconciler<MockUnitControl>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("log").join("log.env");
        let staged = dir.path().join("log").join(STAGED_NAME);
        let checker = dir.path().join("mica-log-env");
        std::fs::write(&checker, "#!/bin/sh\n").unwrap();
        let commands = Arc::new(FakeCommands::default());
        let reconciler = LoggingReconciler::new(
            SYSTEMD_UNIT,
            live.clone(),
            checker.clone(),
            commands.clone(),
            MockUnitControl::new("active", "static"),
        );
        Fixture {
            _dir: dir,
            live,
            staged,
            checker: checker.display().to_string(),
            commands,
            reconciler,
        }
    }

    impl Fixture {
        fn line(&self, settings: &Path) -> String {
            format!(
                "env MICA_LOG_SETTINGS={} {}",
                settings.display(),
                self.checker
            )
        }
    }

    #[test]
    fn unset_values_are_left_to_the_image_and_the_board() {
        assert_eq!(
            render(&logging(None, None, "", RemoteLogProtocol::Udp).logging),
            GOLDEN_UNSET
        );
        let set = render(
            &logging(
                Some(false),
                Some(4096),
                "192.0.2.10",
                RemoteLogProtocol::Tcp,
            )
            .logging,
        );
        assert!(set.ends_with(
            "LOG_PERSISTENT=no\nLOG_MAX_KIB=4096\nLOG_REMOTE=192.0.2.10:514\nLOG_REMOTE_PROTOCOL=tcp\n"
        ));
        let v6 = render(&logging(Some(true), None, "2001:db8::1", RemoteLogProtocol::Udp).logging);
        assert!(
            v6.contains("LOG_PERSISTENT=yes\nLOG_REMOTE=[2001:db8::1]:514\n"),
            "{v6}"
        );
        // A collector kept while forwarding is off is not forwarded to.
        let mut off = logging(None, None, "192.0.2.10", RemoteLogProtocol::Udp);
        off.logging.remote.enabled = false;
        assert_eq!(render(&off.logging), GOLDEN_UNSET);
    }

    #[tokio::test]
    async fn a_render_base_takes_replaces_the_live_file_and_restarts_once() {
        let f = fixture();
        f.commands.answer(
            &f.line(&f.live),
            0,
            "LOG_PERSISTENT=yes\nLOG_MAX_KIB=8192\nLOG_REMOTE=[2001:db8::1]:514\nLOG_REMOTE_PROTOCOL=udp\nLOG_MAX_KIB_LIMIT=65536\n",
        );
        let settings = logging(None, None, "2001:db8::1", RemoteLogProtocol::Udp);
        let state = f.reconciler.apply(&settings).await.unwrap();
        let rendered = std::fs::read_to_string(&f.live).unwrap();
        assert!(rendered.contains("LOG_REMOTE=[2001:db8::1]:514\n"));
        assert!(!rendered.contains("LOG_PERSISTENT"));
        assert!(!f.staged.exists());
        assert_eq!(f.commands.calls(), [f.line(&f.staged), f.line(&f.live)]);
        assert_eq!(
            f.reconciler.control.calls(),
            ["restart mica-log-settings.service"]
        );
        assert_eq!(state["valid"], true);
        assert_eq!(state["persistent"], true);
        assert_eq!(state["persistentSource"], "default");
        assert_eq!(state["maxSizeKiB"], 8192);
        assert_eq!(state["maxSizeKiBLimit"], 65536);
        assert_eq!(
            state["remote"],
            json!({"enabled": true, "address": "2001:db8::1", "port": 514, "protocol": "udp"})
        );
        // The same settings again change nothing and restart nothing.
        f.reconciler.apply(&settings).await.unwrap();
        assert_eq!(
            f.reconciler.control.calls(),
            ["restart mica-log-settings.service"]
        );
        assert_eq!(f.commands.calls().len(), 3);
    }

    #[tokio::test]
    async fn a_render_base_refuses_leaves_the_live_file_and_names_base_s_reason() {
        let f = fixture();
        std::fs::create_dir_all(f.live.parent().unwrap()).unwrap();
        std::fs::write(&f.live, GOLDEN_UNSET).unwrap();
        f.commands.answer_output(
            &f.line(&f.staged),
            Output {
                code: 1,
                stdout: "LOG_PERSISTENT=no\n".into(),
                stderr: "mica-log-env: LOG_MAX_KIB is 131072, outside 1024..16384 (half of DATA/var's limit); the log stays in RAM and nothing is forwarded\n".into(),
            },
        );
        let settings = logging(Some(true), Some(131_072), "", RemoteLogProtocol::Udp);
        let error = f.reconciler.apply(&settings).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("outside 1024..16384"), "{error}");
        assert_eq!(std::fs::read_to_string(&f.live).unwrap(), GOLDEN_UNSET);
        assert!(!f.staged.exists());
        assert!(f.reconciler.control.calls().is_empty());
    }

    #[tokio::test]
    async fn a_root_without_base_s_reader_writes_nothing() {
        let f = fixture();
        std::fs::remove_file(&f.checker).unwrap();
        let state = f
            .reconciler
            .apply(&logging(Some(true), None, "", RemoteLogProtocol::Udp))
            .await
            .unwrap();
        assert_eq!(state["supported"], false);
        assert!(!f.live.exists());
        assert!(f.commands.calls().is_empty());
        assert!(f.reconciler.control.calls().is_empty());
    }
}
