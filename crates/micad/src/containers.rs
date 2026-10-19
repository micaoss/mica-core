//! What the container engine reports, read-only.
//!
//! mica-containerd owns every container (`crate::containerd`), and micad reads
//! its containers from it. This module is what podman itself says: the images
//! on disk, and the containers it lists, which the reconciler reads to see
//! them go before it stops the daemon. It runs `podman` with read-only
//! subcommands, bounded in time and in output, and parses the JSON it prints.
//!
//! **No write ever reaches podman from here.** No `run`, no `rm`, no `pull`:
//! the daemon is the one writer of container state, and a container micad
//! started behind it is one the daemon removes.

use std::time::Duration;

use serde_json::{Value, json};

/// How long one engine call may take before it is abandoned.
///
/// `podman ps` on a device with a cold page cache is not instant, and a
/// management API that blocks on it is worse than one that reports the engine
/// as unavailable.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most output one engine call may produce.
const MAX_OUTPUT: usize = 256 * 1024;

/// The engine binary, as mica-podman installs it.
const PODMAN: &str = "/usr/bin/podman";

/// What the device reports about the containers it runs.
#[async_trait::async_trait]
pub trait ContainerEngine: Send + Sync {
    /// Every container the engine knows, running or not.
    ///
    /// The default is the unavailable one, like every other observer: a
    /// dry-run daemon and a test that did not ask for an engine never reach
    /// the host.
    async fn containers(&self) -> Value {
        unavailable("this build reads no container engine")
    }

    /// The images on disk.
    async fn images(&self) -> Value {
        unavailable("this build reads no container engine")
    }
}

/// The absent answer, in the shape every observer uses for it.
fn unavailable(detail: &str) -> Value {
    json!({ "available": false, "detail": detail })
}

/// The engine as podman answers it.
pub struct Podman {
    binary: String,
}

impl Default for Podman {
    fn default() -> Self {
        Self {
            binary: PODMAN.to_string(),
        }
    }
}

impl Podman {
    /// An engine reading `binary` instead of the installed podman.
    ///
    /// The tests' way in: they point it at a binary that is absent, or at one
    /// that prints something that is not JSON, and assert the refusal.
    #[cfg(test)]
    #[must_use]
    pub fn at(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// One read-only call, bounded in time and output.
    ///
    /// `Err` carries the sentence the caller publishes as the reason the
    /// engine is unavailable; there is no error here that a device operator
    /// can act on beyond "the engine did not answer".
    async fn read(&self, args: &[&str]) -> Result<Value, String> {
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(ENGINE_TIMEOUT, command.output())
            .await
            .map_err(|_| "the container engine did not answer within five seconds".to_string())?
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    format!("{} is not present on this image", self.binary)
                } else {
                    format!("the container engine could not be run: {err}")
                }
            })?;
        if !output.status.success() {
            return Err("the container engine refused a read".to_string());
        }
        if output.stdout.len() > MAX_OUTPUT {
            return Err("the container engine printed more than this build reads".to_string());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|err| format!("the container engine printed something unreadable: {err}"))
    }

    /// One read, wrapped in the available/unavailable envelope.
    async fn observe(&self, args: &[&str], key: &str) -> Value {
        match self.read(args).await {
            Ok(value) => json!({ "available": true, key: value }),
            Err(detail) => {
                tracing::debug!(detail, args = ?args, "container engine unavailable");
                unavailable(&detail)
            }
        }
    }
}

#[async_trait::async_trait]
impl ContainerEngine for Podman {
    async fn containers(&self) -> Value {
        // `--all`, so a container that exited is reported as exited rather
        // than as absent: "it stopped" and "it was never created" are
        // different answers and the console shows different things for them.
        self.observe(&["ps", "--all", "--format", "json"], "entries")
            .await
    }

    async fn images(&self) -> Value {
        self.observe(&["images", "--format", "json"], "entries")
            .await
    }
}

/// An engine that answers nothing, for the dry run and for tests.
pub struct NoEngine;

impl ContainerEngine for NoEngine {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_absent_engine_is_reported_as_absent_and_never_as_empty() {
        let engine = Podman::at("/nonexistent/podman");

        let containers = engine.containers().await;

        assert_eq!(containers["available"], json!(false));
        assert!(
            containers["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("not present")),
            "{containers}"
        );
    }

    #[tokio::test]
    async fn output_that_is_not_json_is_a_refusal_and_not_a_panic() {
        let engine = Podman::at("/bin/echo");

        let value = engine.containers().await;

        assert_eq!(value["available"], json!(false));
        assert!(value["detail"].as_str().is_some(), "{value}");
    }

    #[tokio::test]
    async fn a_build_with_no_engine_answers_the_default() {
        let value = NoEngine.containers().await;

        assert_eq!(value["available"], json!(false));
    }
}
