//! Failure evidence on an OpenRC root: the RAM log through busybox `logread`,
//! and the failed services through Base's `mica-init failed`.

use std::sync::Arc;

use anyhow::Result;

use super::{Commands, Host};
use crate::diagnostics::{FailedUnit, JournalReader, UnitLister};

/// syslog levels at `warning` and worse, as busybox spells them.
const WARNING_OR_WORSE: [&str; 5] = ["emerg", "alert", "crit", "err", "warn"];

/// `logread`, reduced to what `journalctl -p warning` keeps.
pub struct Logread {
    commands: Arc<dyn Commands>,
}

impl Logread {
    pub fn new(commands: Arc<dyn Commands>) -> Self {
        Self { commands }
    }

    pub fn production() -> Self {
        Self::new(Arc::new(Host))
    }
}

/// A busybox syslogd line, `Mmm dd hh:mm:ss <host> <facility>.<level> <rest>`,
/// without its host (the operator's name for the site, not evidence), when
/// its level is warning or worse.
fn warning_line(line: &str) -> Option<String> {
    let (stamp, rest) = (line.get(..15)?, line.get(16..)?);
    let (_host, rest) = rest.split_once(' ')?;
    let (selector, message) = rest.split_once(' ')?;
    let (_facility, level) = selector.split_once('.')?;
    WARNING_OR_WORSE
        .contains(&level)
        .then(|| format!("{stamp} {selector} {message}"))
}

#[async_trait::async_trait]
impl JournalReader for Logread {
    async fn read(&self, max_lines: usize) -> Result<Vec<u8>> {
        let output = self
            .commands
            .run("logread", &[])
            .await?
            .success("logread")?;
        let lines: Vec<String> = output.stdout.lines().filter_map(warning_line).collect();
        let newest = &lines[lines.len().saturating_sub(max_lines)..];
        let mut text = newest.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Ok(text.into_bytes())
    }
}

/// The failed and crashed services, `mica-init failed`, one a line.
pub struct InitFailedUnits {
    commands: Arc<dyn Commands>,
}

impl InitFailedUnits {
    pub fn new(commands: Arc<dyn Commands>) -> Self {
        Self { commands }
    }

    pub fn production() -> Self {
        Self::new(Arc::new(Host))
    }
}

#[async_trait::async_trait]
impl UnitLister for InitFailedUnits {
    async fn failed_units(&self) -> Result<Vec<FailedUnit>> {
        let output = self
            .commands
            .run("/usr/lib/mica/mica-init", &["failed"])
            .await?
            .success("mica-init failed")?;
        Ok(output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| FailedUnit {
                name: name.to_string(),
                description: String::new(),
                load_state: "loaded".to_string(),
                active_state: "failed".to_string(),
                sub_state: "failed".to_string(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrc::FakeCommands;

    const LOG: &str = "\
Sep 28 01:00:00 site-name daemon.info micad[10]: started
Sep 28 01:00:01 site-name daemon.warn micad[10]: a warning
Sep 28 01:00:02 site-name kern.err kernel: an error
Sep 28 01:00:03 site-name auth.notice login: notice
Sep 28 01:00:04 site-name daemon.crit apid[11]: critical
";

    #[tokio::test]
    async fn the_log_keeps_warnings_and_worse_without_the_host() {
        let fake = Arc::new(FakeCommands::default());
        fake.answer("logread", 0, LOG);
        let reader = Logread::new(Arc::clone(&fake) as Arc<dyn Commands>);
        let text = String::from_utf8(reader.read(400).await.unwrap()).unwrap();
        assert_eq!(
            text,
            "Sep 28 01:00:01 daemon.warn micad[10]: a warning\n\
             Sep 28 01:00:02 kern.err kernel: an error\n\
             Sep 28 01:00:04 daemon.crit apid[11]: critical\n"
        );
        assert!(!text.contains("site-name"));
        let newest = String::from_utf8(reader.read(1).await.unwrap()).unwrap();
        assert_eq!(newest, "Sep 28 01:00:04 daemon.crit apid[11]: critical\n");
    }

    #[tokio::test]
    async fn failed_services_come_from_mica_init() {
        let fake = Arc::new(FakeCommands::default());
        fake.answer("/usr/lib/mica/mica-init failed", 0, "dropbear\nmica-ntpd\n");
        let units = InitFailedUnits::new(Arc::clone(&fake) as Arc<dyn Commands>)
            .failed_units()
            .await
            .unwrap();
        let names: Vec<_> = units.iter().map(|unit| unit.name.as_str()).collect();
        assert_eq!(names, ["dropbear", "mica-ntpd"]);
        assert!(units.iter().all(|unit| unit.active_state == "failed"));
    }
}
