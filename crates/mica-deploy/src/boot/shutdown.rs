//! One bounded storage-release policy for exitrd and partial startup refusal.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod release;
mod snapshot;
mod supervisor;
mod system;
mod worker;
pub use release::*;
pub use snapshot::*;
pub use supervisor::*;
pub use system::*;
pub use worker::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Reboot,
    Poweroff,
    Halt,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reboot => "reboot",
            Self::Poweroff => "poweroff",
            Self::Halt => "halt",
        }
    }
    pub fn parse(args: &[String]) -> Result<Self> {
        Ok(Request::parse(args)?.action)
    }
}

pub struct Request {
    pub action: Action,
    pub timeout_ms: Option<u64>,
}
impl Request {
    pub fn parse(args: &[String]) -> Result<Self> {
        ensure!(
            !args.is_empty()
                && args.len() <= 16
                && args.iter().map(String::len).sum::<usize>() <= 1024,
            "invalid shutdown arguments"
        );
        let action = match args[0].as_str() {
            "reboot" => Action::Reboot,
            "poweroff" => Action::Poweroff,
            "halt" => Action::Halt,
            _ => bail!("unsupported shutdown action"),
        };
        let mut seen = BTreeSet::new();
        let mut timeout_ms = None;
        let mut iter = args[1..].iter();
        while let Some(arg) = iter.next() {
            let (key, value) = if let Some(pair) = arg.split_once('=') {
                pair
            } else if ["--log-color", "--log-location", "--log-time"].contains(&arg.as_str()) {
                (arg.as_str(), "true")
            } else {
                (
                    arg.as_str(),
                    iter.next()
                        .context("missing shutdown metadata value")?
                        .as_str(),
                )
            };
            ensure!(seen.insert(key), "duplicate shutdown metadata");
            let level = |s: &str| {
                [
                    "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug",
                ]
                .contains(&s)
                    || s.parse::<u8>().is_ok_and(|n| n <= 7)
            };
            let target = |s: &str| {
                [
                    "console",
                    "console-prefixed",
                    "kmsg",
                    "syslog",
                    "syslog-or-kmsg",
                    "journal",
                    "journal-or-kmsg",
                    "auto",
                    "null",
                ]
                .contains(&s)
            };
            let valid = match key {
                "--log-level" => {
                    value.split(',').count() <= 8
                        && value.split(',').all(|part| {
                            part.split_once(':').map_or_else(
                                || level(part),
                                |(name, value)| target(name) && level(value),
                            )
                        })
                }
                "--log-target" => target(value),
                "--log-color" | "--log-location" | "--log-time" => {
                    ["0", "1", "yes", "no", "true", "false"].contains(&value)
                }
                "--exit-code" => value.parse::<u8>().is_ok(),
                "--timeout" => {
                    let (number, multiplier) = if let Some(n) = value.strip_suffix("us") {
                        (n, 1)
                    } else if let Some(n) = value.strip_suffix("ms") {
                        (n, 1000)
                    } else {
                        (value.strip_suffix('s').unwrap_or(value), 1_000_000)
                    };
                    let micros = number
                        .parse::<u64>()?
                        .checked_mul(multiplier)
                        .context("shutdown metadata timeout overflow")?;
                    ensure!(
                        (1000..=3_600_000_000).contains(&micros),
                        "invalid shutdown metadata timeout"
                    );
                    timeout_ms = Some(micros / 1000);
                    true
                }
                _ => false,
            };
            ensure!(valid, "unsupported shutdown metadata");
        }
        Ok(Self { action, timeout_ms })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub deadline_ms: u64,
    pub cleanup_deadline_ms: u64,
}
impl Budget {
    pub fn new(now: u64, watchdog_seconds: u64) -> Result<Self> {
        ensure!(
            (20..=86400).contains(&watchdog_seconds),
            "inadequate watchdog timeout"
        );
        let duration = (watchdog_seconds - 10).min(60) * 1000;
        let deadline_ms = now.checked_add(duration).context("deadline overflow")?;
        Ok(Self {
            deadline_ms,
            cleanup_deadline_ms: deadline_ms - 2000,
        })
    }
    pub fn operation_deadline(self, now: u64) -> Result<u64> {
        ensure!(now < self.cleanup_deadline_ms, "cleanup deadline exhausted");
        Ok(now.saturating_add(5000).min(self.cleanup_deadline_ms))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Device {
    pub major: u32,
    pub minor: u32,
}
impl Device {
    fn parse(value: &str) -> Result<Self> {
        let (major, minor) = value.split_once(':').context("invalid device identity")?;
        Ok(Self {
            major: major.parse()?,
            minor: minor.parse()?,
        })
    }
    pub fn from_raw(value: u64) -> Self {
        Self {
            major: rustix::fs::major(value),
            minor: rustix::fs::minor(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub id: u64,
    pub parent: u64,
    pub device: Device,
    pub root: String,
    pub path: String,
    pub kind: String,
    pub propagation: Vec<String>,
}

fn unescape(value: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut input = value.as_bytes();
    while !input.is_empty() {
        if input[0] == b'\\' {
            ensure!(input.len() >= 4, "invalid mount escape");
            bytes.push(match &input[1..4] {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => bail!("invalid mount escape"),
            });
            input = &input[4..];
        } else {
            ensure!(input[0] != 0, "invalid mount path");
            bytes.push(input[0]);
            input = &input[1..];
        }
    }
    let value = String::from_utf8(bytes)?;
    ensure!(
        value.starts_with('/') && value.len() <= 4096,
        "invalid mount path"
    );
    Ok(value)
}

pub fn parse_mountinfo(text: &str) -> Result<Vec<Mount>> {
    ensure!(
        !text.is_empty() && text.len() <= 1024 * 1024 && text.lines().count() <= 4096,
        "invalid mountinfo size"
    );
    let mut mounts = Vec::new();
    let mut parents = BTreeMap::new();
    for line in text.lines() {
        let (left, right) = line.split_once(" - ").context("malformed mountinfo")?;
        let left: Vec<_> = left.split(' ').collect();
        let right: Vec<_> = right.split(' ').collect();
        ensure!(
            left.len() >= 6 && right.len() == 3 && left.iter().chain(&right).all(|x| !x.is_empty()),
            "malformed mountinfo fields"
        );
        let id = left[0].parse::<u64>()?;
        let parent = left[1].parse::<u64>()?;
        ensure!(
            id != 0 && (id != parent || left[4] == "/") && parents.insert(id, parent).is_none(),
            "invalid mount ID"
        );
        mounts.push(Mount {
            id,
            parent,
            device: Device::parse(left[2])?,
            root: unescape(left[3])?,
            path: unescape(left[4])?,
            kind: right[0].into(),
            propagation: left[6..].iter().map(|s| (*s).into()).collect(),
        });
    }
    ensure!(
        mounts.iter().any(|m| m.path == "/"),
        "mountinfo has no root"
    );
    for mount in &mounts {
        let mut current = mount.id;
        let mut visited = BTreeSet::new();
        while let Some(&parent) = parents.get(&current) {
            ensure!(visited.insert(current), "cyclic mountinfo");
            if current == parent {
                break;
            }
            current = parent;
        }
    }
    Ok(mounts)
}

#[cfg(test)]
mod tests;
