//! Compose projects: the check mica-containerd runs on a file.
//!
//! `mica-containerd compose check` holds a Compose file to the daemon's rules
//! with no daemon (mica-podman's README, *The API*). micad runs it on every
//! project a write adds or changes, and for the console's check.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

/// The daemon's binary, as mica-podman installs it.
pub const CONTAINERD_BINARY: &str = "/usr/bin/mica-containerd";
/// How long one check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// The most output one check is read for.
const MAX_OUTPUT: usize = 1024 * 1024;

/// What checks a project's Compose text.
#[async_trait::async_trait]
pub trait ComposeCheck: Send + Sync {
    /// The check of `text` as project `project`, its relative host paths in
    /// `dir` or else in `/mica/volumes/<project>/`: `{"ok": true,
    /// "containers"}` or `{"ok": false, "detail", "problems"}`. `Err` when it
    /// could not run.
    async fn check(&self, project: &str, text: &str, dir: Option<&str>) -> Result<Value, String>;
}

/// The check of the installed mica-containerd.
pub struct ContainerdCompose {
    binary: PathBuf,
}

impl ContainerdCompose {
    /// The installed binary.
    #[must_use]
    pub fn production() -> Self {
        Self::at(CONTAINERD_BINARY)
    }

    /// Another binary: the tests' fake.
    #[must_use]
    pub fn at(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    async fn run(&self, args: &[&str], stdin: &str) -> Result<std::process::Output, String> {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let run = async {
            let mut child = command
                .spawn()
                .map_err(|err| format!("run {}: {err}", self.binary.display()))?;
            if let Some(mut pipe) = child.stdin.take() {
                pipe.write_all(stdin.as_bytes())
                    .await
                    .map_err(|err| format!("write the file to {}: {err}", self.binary.display()))?;
            }
            child
                .wait_with_output()
                .await
                .map_err(|err| format!("run {}: {err}", self.binary.display()))
        };
        let output = tokio::time::timeout(CHECK_TIMEOUT, run)
            .await
            .map_err(|_| {
                format!(
                    "{} did not answer within {} seconds",
                    self.binary.display(),
                    CHECK_TIMEOUT.as_secs()
                )
            })??;
        if output.stdout.len() > MAX_OUTPUT || output.stderr.len() > MAX_OUTPUT {
            return Err(format!(
                "{} printed more than this build reads",
                self.binary.display()
            ));
        }
        Ok(output)
    }
}

#[async_trait::async_trait]
impl ComposeCheck for ContainerdCompose {
    async fn check(&self, project: &str, text: &str, dir: Option<&str>) -> Result<Value, String> {
        let mut args = vec!["compose", "check", "--project", project];
        if let Some(dir) = dir {
            args.extend(["--project-dir", dir]);
        }
        args.push("-");
        let output = self.run(&args, text).await?;
        let parse = |bytes: &[u8]| {
            serde_json::from_slice::<Value>(bytes).map_err(|err| {
                format!("mica-containerd compose check printed something unreadable: {err}")
            })
        };
        match output.status.code() {
            Some(0) => {
                let answer = parse(&output.stdout)?;
                Ok(json!({ "ok": true, "containers": answer["containers"] }))
            }
            Some(1) => {
                let refusal = parse(&output.stderr)?;
                Ok(json!({
                    "ok": false,
                    "detail": refusal["detail"],
                    "problems": refusal["problems"],
                }))
            }
            _ => Err(format!(
                "mica-containerd compose check failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        }
    }
}

/// A build that checks no file: the dry run, and tests that ask for none.
pub struct NoCompose;

#[async_trait::async_trait]
impl ComposeCheck for NoCompose {
    async fn check(
        &self,
        _project: &str,
        _text: &str,
        _dir: Option<&str>,
    ) -> Result<Value, String> {
        Err("this build checks no Compose file".into())
    }
}

/// The sentence a refused check is written as: the project and its problems.
#[must_use]
pub fn refusal(project: &str, outcome: &Value) -> Option<String> {
    (outcome["ok"] != json!(true)).then(|| {
        format!(
            "container project {project:?} is refused: {}",
            outcome["detail"]
                .as_str()
                .unwrap_or("the Compose file is refused")
        )
    })
}

#[cfg(test)]
pub(crate) mod tests;
