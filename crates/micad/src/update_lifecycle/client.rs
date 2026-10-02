//! The deployment client micad drives: `mica-deploy` as a subprocess, or none.

use anyhow::{Context, Result, ensure};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// What one client invocation produced.
#[derive(Debug)]
pub struct ClientOutput {
    /// Process exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The client binary is not there to run. Its own error type so the caller
/// can say "client unavailable" instead of a generic failure.
#[derive(Debug)]
pub struct ClientUnavailable(pub String);

impl std::fmt::Display for ClientUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "update client unavailable: {}", self.0)
    }
}

impl std::error::Error for ClientUnavailable {}

/// Runs `mica-deploy` (or a stand-in) with an argument vector.
///
/// One method rather than one per subcommand: the argument rendering and the
/// output parsing are pure functions tested on their own, so the trait's only
/// job is "run this and give me what it said" — which is exactly what a mock
/// has to fake and nothing more.
#[async_trait::async_trait]
pub trait UpdateClient: Send + Sync {
    /// Whether the client can run at all; `Some(reason)` when it cannot.
    fn unavailable(&self) -> Option<String>;

    /// Run one invocation, bounded by `timeout`.
    async fn run(&self, args: &[String], timeout: Duration) -> Result<ClientOutput>;
}

/// Production client: spawn the configured binary as a subprocess.
pub struct SubprocessClient {
    pub(super) binary: PathBuf,
}

pub(super) struct ClientProcessGroup(pub(super) Option<rustix::process::Pid>);
impl Drop for ClientProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

impl SubprocessClient {
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
}

#[async_trait::async_trait]
impl UpdateClient for SubprocessClient {
    fn unavailable(&self) -> Option<String> {
        if self.binary.is_file() {
            None
        } else {
            Some(format!(
                "{} is not present on this image",
                self.binary.display()
            ))
        }
    }

    async fn run(&self, args: &[String], timeout: Duration) -> Result<ClientOutput> {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::new(ClientUnavailable(format!(
                    "{} is not present on this image",
                    self.binary.display()
                )))
            } else {
                anyhow::Error::from(err).context(format!("spawn {}", self.binary.display()))
            }
        })?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            .context("missing client process ID")?;
        let mut group = ClientProcessGroup(Some(group));
        let stdout = child.stdout.take().context("missing client stdout")?;
        let stderr = child.stderr.take().context("missing client stderr")?;
        let result = tokio::time::timeout(timeout, async {
            tokio::try_join!(
                read_client_output(stdout, 65536),
                read_client_output(stderr, 16384),
                async { Ok::<_, anyhow::Error>(child.wait().await?) }
            )
        })
        .await
        .context("update client exceeded its deadline")
        .and_then(|result| result);
        let (stdout, stderr, status) = match result {
            Ok(output) => output,
            Err(error) => {
                drop(group);
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(error);
            }
        };
        group.0 = None;
        Ok(ClientOutput {
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

pub(super) async fn read_client_output(
    stream: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= limit, "update client output exceeds bound");
    Ok(bytes)
}

/// The client a daemon that was never handed one has: none. The default in
/// [`crate::bus::MicadService`], so a dry-run daemon can neither spawn a
/// process nor claim it could.
pub struct NoClient;

#[async_trait::async_trait]
impl UpdateClient for NoClient {
    fn unavailable(&self) -> Option<String> {
        Some("this daemon was started without an update client".to_string())
    }

    async fn run(&self, _args: &[String], _timeout: Duration) -> Result<ClientOutput> {
        Err(anyhow::Error::new(ClientUnavailable(
            "this daemon was started without an update client".to_string(),
        )))
    }
}
