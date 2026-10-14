//! OpenRC: what micad runs where a systemd product talks to systemd's D-Bus
//! services. Every backend here is a command the OpenRC root ships -- OpenRC's
//! own (`rc-service`, `openrc-shutdown`), Base's `mica-init`, and busybox's
//! (`logread`, `ifup`, `ifdown`, `ip`) -- run through [`Commands`], so each one
//! is tested against a fake runner.

use std::time::Duration;

use anyhow::{Context, Result};

mod hostname;
mod logs;
mod network_state;
mod power;
mod time_status;
mod units;
pub use hostname::EtcHostname;
pub use logs::{InitFailedUnits, Logread};
pub use network_state::IpNetworkState;
pub use power::OpenrcPower;
pub use time_status::NtpdTimeStatus;
pub use units::OpenrcUnits;

/// How long one command may run before it is abandoned.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// What a finished command said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// The exit code; -1 when a signal ended it.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// The output of a command that exited 0, or an error naming it.
    ///
    /// # Errors
    ///
    /// A non-zero exit, with the command and its stderr.
    pub fn success(self, command: &str) -> Result<Self> {
        anyhow::ensure!(
            self.code == 0,
            "{command} exited {}: {}",
            self.code,
            self.stderr.trim()
        );
        Ok(self)
    }
}

/// Runs a command to completion.
#[async_trait::async_trait]
pub trait Commands: Send + Sync {
    /// Run `program` with `args`; a non-zero exit is an [`Output`], not an
    /// error.
    ///
    /// # Errors
    ///
    /// The command could not be run or did not finish in time.
    async fn run(&self, program: &str, args: &[&str]) -> Result<Output>;
}

/// The host's commands, each bounded by [`COMMAND_TIMEOUT`] and killed when
/// abandoned.
#[derive(Debug, Clone, Copy, Default)]
pub struct Host;

#[async_trait::async_trait]
impl Commands for Host {
    async fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let output = tokio::time::timeout(
            COMMAND_TIMEOUT,
            tokio::process::Command::new(program)
                .args(args)
                .env("LC_ALL", "C")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .with_context(|| format!("{program} did not finish within {COMMAND_TIMEOUT:?}"))?
        .with_context(|| format!("run {program}"))?;
        Ok(Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A scripted [`Commands`] for the tests: it records every command line and
/// answers each with the output registered for it, or exit 0 and no output.
#[cfg(test)]
#[derive(Default)]
pub struct FakeCommands {
    calls: std::sync::Mutex<Vec<String>>,
    answers: std::sync::Mutex<std::collections::BTreeMap<String, Output>>,
}

#[cfg(test)]
impl FakeCommands {
    /// Answer the command line `line` (`program arg...`) with `code` and
    /// `stdout`.
    pub fn answer(&self, line: &str, code: i32, stdout: &str) {
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                line.to_string(),
                Output {
                    code,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                },
            );
    }

    /// Every command line run so far, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl Commands for FakeCommands {
    async fn run(&self, program: &str, args: &[&str]) -> Result<Output> {
        let line = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.clone());
        Ok(self
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&line)
            .cloned()
            .unwrap_or_default())
    }
}
