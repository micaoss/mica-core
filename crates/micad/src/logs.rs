//! A read-only view of one service's log, for the console.
//!
//! Bounded on every axis the diagnostics excerpt is -- lines, bytes, bytes a
//! line, time -- and limited to an allowlist of the services this device runs
//! for its operator, so the route cannot read what any process ever logged.
//! systemd's journal (`journalctl -u`) on one init, Base's `logread` (by
//! syslog tag, the host dropped) on the other; of this boot or of the one
//! before it, when the log keeps it. apid scrubs every line before it leaves
//! the device.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use micad_settings::{Feature, Init};
use serde_json::{Value, json};

use crate::diagnostics::{JOURNAL_MAX_BYTES, JOURNAL_MAX_LINE_BYTES, bound_excerpt};
use crate::openrc::{Commands, Host};

/// The newest lines a read returns.
pub const LOG_MAX_LINES: usize = 200;
/// How long a read may take before it is refused.
const LOG_TIMEOUT: Duration = Duration::from_secs(5);

/// One readable log: the name the API uses, the feature it belongs to, the
/// systemd unit (a glob where the service is a template) and the syslog tag
/// its process logs under on OpenRC.
#[derive(Debug)]
pub struct LogSource<'a> {
    pub name: &'a str,
    pub feature: Option<Feature>,
    pub unit: &'a str,
    pub tag: &'a str,
}

/// The boot a read covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    Current,
    Previous,
}

impl Boot {
    /// The boot named `name`, `current` or `previous`.
    ///
    /// # Errors
    ///
    /// Any other name.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "current" => Ok(Self::Current),
            "previous" => Ok(Self::Previous),
            _ => bail!("no boot named {name:?}; a log is of the current or the previous boot"),
        }
    }

    /// How the snapshot names the excerpt's scope.
    #[must_use]
    pub fn scope(self) -> &'static str {
        match self {
            Self::Current => "current boot",
            Self::Previous => "previous boot",
        }
    }
}

/// A read of the previous boot when the log keeps none: it is in RAM, or
/// this is the first boot it is kept for. Not a failure; the answer says so.
#[derive(Debug)]
pub struct NoPreviousBoot;

impl std::fmt::Display for NoPreviousBoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no previous boot is kept (persistent logs are off, or none was kept yet)")
    }
}

impl std::error::Error for NoPreviousBoot {}

/// The reason a read failed, `prefix` before it unless no previous boot is
/// kept, which is its own answer.
pub(crate) fn read_failure(prefix: &str, err: &anyhow::Error) -> String {
    match err.downcast_ref::<NoPreviousBoot>() {
        Some(none) => none.to_string(),
        None => format!("{prefix}: {err:#}"),
    }
}

/// `journalctl` of `boot` with `args` after the common ones (no pager, no
/// hostname, ISO stamps). The previous boot is read only when
/// `--list-boots` names one: a journal in RAM may answer `-b -1` with this
/// boot.
pub(crate) async fn journalctl(
    commands: &dyn Commands,
    boot: Boot,
    args: &[&str],
) -> Result<Vec<u8>> {
    let mut all = vec!["-b"];
    if boot == Boot::Previous {
        let boots = commands
            .run("journalctl", &["--list-boots", "-q", "--no-pager"])
            .await?
            .success("journalctl --list-boots")?;
        let kept = boots
            .stdout
            .lines()
            .any(|line| line.split_whitespace().next() == Some("-1"));
        if !kept {
            return Err(NoPreviousBoot.into());
        }
        all.push("-1");
    }
    all.extend(["-q", "--no-pager", "--no-hostname", "-o", "short-iso"]);
    all.extend(args);
    let output = commands
        .run("journalctl", &all)
        .await?
        .success("journalctl")?;
    Ok(output.stdout.into_bytes())
}

/// Base's `logread` of `boot`: from its `mica-boot` marker, the previous
/// boot empty when none is kept. A Base older than the markers has busybox's
/// `logread`, which refuses `-b`; this boot is then the whole RAM log.
pub(crate) async fn logread(commands: &dyn Commands, boot: Boot) -> Result<String> {
    match boot {
        Boot::Current => {
            let output = commands.run("logread", &["-b", "0"]).await?;
            if output.code == 0 {
                return Ok(output.stdout);
            }
            Ok(commands
                .run("logread", &[])
                .await?
                .success("logread")?
                .stdout)
        }
        Boot::Previous => {
            let output = commands
                .run("logread", &["-b", "-1"])
                .await?
                .success("logread -b -1")?;
            if output.stdout.trim().is_empty() {
                return Err(NoPreviousBoot.into());
            }
            Ok(output.stdout)
        }
    }
}

/// Every log the console may read.
pub const LOG_SOURCES: [LogSource<'static>; 9] = [
    LogSource {
        name: "micad",
        feature: None,
        unit: "micad.service",
        tag: "micad",
    },
    LogSource {
        name: "apid",
        feature: None,
        unit: "apid.service",
        tag: "mica-apid",
    },
    LogSource {
        name: "time",
        feature: None,
        unit: "systemd-timesyncd.service",
        tag: "ntpd",
    },
    LogSource {
        name: "ssh",
        feature: Some(Feature::Ssh),
        unit: "mica-sshd.service",
        tag: "mica-sshd",
    },
    LogSource {
        name: "wifi-client",
        feature: Some(Feature::Wifi),
        unit: "wpa_supplicant@*.service",
        tag: "wpa_supplicant",
    },
    LogSource {
        name: "wifi-ap",
        feature: Some(Feature::Wifi),
        unit: "hostapd@*.service",
        tag: "hostapd",
    },
    LogSource {
        name: "bluetooth",
        feature: Some(Feature::Bluetooth),
        unit: "bluetooth.service",
        tag: "bluetoothd",
    },
    LogSource {
        name: "mqtt-broker",
        feature: Some(Feature::Mqtt),
        unit: "mica-mqtt-broker.service",
        tag: "mica-mqtt-broker",
    },
    LogSource {
        name: "mqttd",
        feature: Some(Feature::Mqtt),
        unit: "mica-mqttd.service",
        tag: "mica-mqttd",
    },
];

/// The source named `name`, when the product serves its feature.
///
/// # Errors
///
/// A name the allowlist does not carry, or a feature the product leaves out.
pub fn source(
    name: &str,
    features: &micad_settings::Features,
) -> Result<&'static LogSource<'static>> {
    let Some(source) = LOG_SOURCES.iter().find(|source| source.name == name) else {
        bail!(
            "no log named {name:?}; the readable logs are {}",
            names().join(", ")
        );
    };
    if let Some(feature) = source.feature
        && !features.has(feature)
    {
        bail!("this product does not carry {feature}, so it has no {name} log");
    }
    Ok(source)
}

fn names() -> Vec<&'static str> {
    LOG_SOURCES.iter().map(|source| source.name).collect()
}

/// Reads one source's newest lines of one boot, raw.
#[async_trait::async_trait]
pub trait LogReader: Send + Sync {
    async fn read(&self, source: &LogSource<'_>, max_lines: usize, boot: Boot) -> Result<Vec<u8>>;
}

/// The reader a daemon without host access has: none.
pub struct NoLogs;

#[async_trait::async_trait]
impl LogReader for NoLogs {
    async fn read(
        &self,
        _source: &LogSource<'_>,
        _max_lines: usize,
        _boot: Boot,
    ) -> Result<Vec<u8>> {
        bail!("this daemon reads no logs")
    }
}

/// The reader of this root's init.
pub struct HostLogs {
    init: Init,
    commands: Arc<dyn Commands>,
}

impl HostLogs {
    pub fn new(init: Init, commands: Arc<dyn Commands>) -> Self {
        Self { init, commands }
    }

    pub fn production(init: Init) -> Self {
        Self::new(init, Arc::new(Host))
    }
}

/// A busybox syslogd line of `tag`, `Mmm dd hh:mm:ss <host> <selector>
/// <tag>[pid]: message`, without its host.
fn tagged_line(line: &str, tag: &str) -> Option<String> {
    let (stamp, rest) = (line.get(..15)?, line.get(16..)?);
    let (_host, rest) = rest.split_once(' ')?;
    let (selector, message) = rest.split_once(' ')?;
    let program = message.split([':', '[']).next()?;
    (program == tag).then(|| format!("{stamp} {selector} {message}"))
}

#[async_trait::async_trait]
impl LogReader for HostLogs {
    async fn read(&self, source: &LogSource<'_>, max_lines: usize, boot: Boot) -> Result<Vec<u8>> {
        match self.init {
            Init::Systemd => {
                let lines = max_lines.to_string();
                journalctl(
                    self.commands.as_ref(),
                    boot,
                    &["-u", source.unit, "-n", &lines],
                )
                .await
            }
            Init::Openrc => {
                let log = logread(self.commands.as_ref(), boot).await?;
                let lines: Vec<String> = log
                    .lines()
                    .filter_map(|line| tagged_line(line, source.tag))
                    .collect();
                let newest = &lines[lines.len().saturating_sub(max_lines)..];
                Ok(newest
                    .iter()
                    .flat_map(|line| format!("{line}\n").into_bytes())
                    .collect())
            }
        }
    }
}

/// The `GetLog` answer for `source`: its newest lines of this boot, bounded,
/// or the reason they could not be read.
pub async fn log_json(reader: &dyn LogReader, source: &LogSource<'_>) -> Value {
    log_of_boot_json(reader, source, Boot::Current).await
}

/// The `GetLogOfBoot` answer: [`log_json`] of `boot`.
pub async fn log_of_boot_json(reader: &dyn LogReader, source: &LogSource<'_>, boot: Boot) -> Value {
    let read = tokio::time::timeout(LOG_TIMEOUT, reader.read(source, LOG_MAX_LINES, boot)).await;
    let base = json!({ "source": source.name });
    let mut value = match read {
        Err(_) => micad_settings::absent(format!("the log did not answer within {LOG_TIMEOUT:?}")),
        Ok(Err(err)) => micad_settings::absent(read_failure("the log could not be read", &err)),
        Ok(Ok(raw)) => {
            let excerpt = bound_excerpt(
                &raw,
                LOG_MAX_LINES,
                JOURNAL_MAX_BYTES,
                JOURNAL_MAX_LINE_BYTES,
            );
            json!({
                "available": true,
                "lines": excerpt.lines,
                "truncated": excerpt.truncated,
            })
        }
    };
    if let (Some(value), Some(base)) = (value.as_object_mut(), base.as_object()) {
        value.extend(base.clone());
    }
    value
}

#[cfg(test)]
mod tests;
