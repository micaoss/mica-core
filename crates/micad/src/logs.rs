//! A read-only view of one service's log, for the console.
//!
//! Bounded on every axis the diagnostics excerpt is -- lines, bytes, bytes a
//! line, time -- and limited to an allowlist of the services this device runs
//! for its operator, so the route cannot read what any process ever logged.
//! systemd's journal (`journalctl -u`) on one init, the RAM log (`logread`,
//! by syslog tag, the host dropped) on the other. apid scrubs every line
//! before it leaves the device.

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
pub struct LogSource {
    pub name: &'static str,
    pub feature: Option<Feature>,
    pub unit: &'static str,
    pub tag: &'static str,
}

/// Every log the console may read.
pub const LOG_SOURCES: [LogSource; 9] = [
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
        unit: "dropbear.service",
        tag: "dropbear",
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
pub fn source<'a>(name: &str, features: &micad_settings::Features) -> Result<&'a LogSource> {
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

/// Reads one source's newest lines, raw.
#[async_trait::async_trait]
pub trait LogReader: Send + Sync {
    async fn read(&self, source: &LogSource, max_lines: usize) -> Result<Vec<u8>>;
}

/// The reader a daemon without host access has: none.
pub struct NoLogs;

#[async_trait::async_trait]
impl LogReader for NoLogs {
    async fn read(&self, _source: &LogSource, _max_lines: usize) -> Result<Vec<u8>> {
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
    async fn read(&self, source: &LogSource, max_lines: usize) -> Result<Vec<u8>> {
        match self.init {
            Init::Systemd => {
                let lines = max_lines.to_string();
                let output = self
                    .commands
                    .run(
                        "journalctl",
                        &[
                            "-b",
                            "-q",
                            "--no-pager",
                            "--no-hostname",
                            "-o",
                            "short-iso",
                            "-u",
                            source.unit,
                            "-n",
                            &lines,
                        ],
                    )
                    .await?
                    .success("journalctl")?;
                Ok(output.stdout.into_bytes())
            }
            Init::Openrc => {
                let output = self
                    .commands
                    .run("logread", &[])
                    .await?
                    .success("logread")?;
                let lines: Vec<String> = output
                    .stdout
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

/// The `GetLog` answer for `source`: its newest lines, bounded, or the reason
/// they could not be read.
pub async fn log_json(reader: &dyn LogReader, source: &LogSource) -> Value {
    let read = tokio::time::timeout(LOG_TIMEOUT, reader.read(source, LOG_MAX_LINES)).await;
    let base = json!({ "source": source.name });
    let mut value = match read {
        Err(_) => micad_settings::absent(format!("the log did not answer within {LOG_TIMEOUT:?}")),
        Ok(Err(err)) => micad_settings::absent(format!("the log could not be read: {err:#}")),
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
