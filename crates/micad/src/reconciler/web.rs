//! Web reconciler: whether apid runs, and where it listens.
//!
//! Renders `access.web` into the file apid reads at start, then keeps apid
//! running for `network` and `local` and stopped for `off`; a running apid is
//! restarted when the file changed. Nothing enables apid at boot: this
//! reconciler starts it, after micad claims its bus name.

use std::path::PathBuf;

use anyhow::{Context, Result};
use micad_settings::{APID_LISTENERS_PATH, Settings, WebMode};
use serde_json::json;

use super::Reconciler;
use super::systemd::{UnitControl, is_active};
use crate::fswrite::write_config_if_changed;

/// The unit apid runs as.
const APID_UNIT: &str = "apid.service";
/// World-readable, owner-writable: it names ports, nothing secret.
const LISTENERS_MODE: u32 = 0o644;
/// `ActiveState` of a unit systemd has given up on.
const FAILED_STATE: &str = "failed";

/// Reconciler for the `access.web` settings subtree.
pub struct WebReconciler<C: UnitControl> {
    path: PathBuf,
    control: C,
}

impl<C: UnitControl> WebReconciler<C> {
    /// Render to `path` and drive apid through `control`.
    pub fn new(path: PathBuf, control: C) -> Self {
        Self { path, control }
    }

    /// Render to the production path.
    pub fn production(control: C) -> Self {
        Self::new(PathBuf::from(APID_LISTENERS_PATH), control)
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for WebReconciler<C> {
    fn name(&self) -> &'static str {
        "web"
    }

    fn subtree(&self) -> &'static str {
        "access.web"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let web = settings.access.web;
        let mut rendered = serde_json::to_string_pretty(&web).context("render apid listeners")?;
        rendered.push('\n');
        let changed = write_config_if_changed(&self.path, &rendered, LISTENERS_MODE)
            .with_context(|| format!("write {}", self.path.display()))?;
        // The apid that asked for the change answers before this runs: the
        // write is an apply task, and the restart or stop follows the answer.
        let state = self.control.active_state(APID_UNIT).await?;
        let action = match (web.mode, is_active(&state)) {
            (WebMode::Off, true) => {
                self.control.stop(APID_UNIT).await?;
                "stopped"
            }
            (WebMode::Off, false) => "none",
            (_, false) => {
                if state == FAILED_STATE {
                    self.control.reset_failed(APID_UNIT).await?;
                }
                self.control.start(APID_UNIT).await?;
                "started"
            }
            (_, true) if changed => {
                self.control.restart(APID_UNIT).await?;
                "restarted"
            }
            (_, true) => "none",
        };
        Ok(json!({
            "mode": web.mode.as_str(),
            "httpPort": web.http_port,
            "httpsEnabled": web.https_enabled,
            "httpsPort": web.https_port,
            "apid": action,
        }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::reconciler::systemd::mock::MockUnitControl;

    fn reconciler(dir: &tempfile::TempDir, active: &str) -> WebReconciler<MockUnitControl> {
        WebReconciler::new(
            dir.path().join("apid.json"),
            MockUnitControl::new(active, "disabled"),
        )
    }

    fn calls(web: &WebReconciler<MockUnitControl>, call: &str) -> usize {
        web.control
            .calls()
            .iter()
            .filter(|made| made.as_str() == call)
            .count()
    }

    /// A boot: the file is rendered, then apid, not yet running, is started.
    #[tokio::test]
    async fn a_boot_renders_the_file_then_starts_apid() {
        let dir = tempfile::tempdir().unwrap();
        let web = reconciler(&dir, "inactive");
        let state = web.apply(&Settings::default()).await.unwrap();
        assert_eq!(state["apid"], json!("started"));
        assert_eq!(state["mode"], json!("network"));
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("apid.json")).unwrap())
                .unwrap();
        assert_eq!(
            file,
            json!({ "mode": "network", "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 })
        );
        assert_eq!(calls(&web, "start apid.service"), 1);
        assert_eq!(calls(&web, "restart apid.service"), 0);
    }

    /// A running apid is restarted when its listeners change, and only then.
    #[tokio::test]
    async fn a_running_apid_is_restarted_only_when_its_listeners_change() {
        let dir = tempfile::tempdir().unwrap();
        let web = reconciler(&dir, "active");
        let mut settings = Settings::default();
        web.apply(&settings).await.unwrap();
        assert_eq!(
            calls(&web, "restart apid.service"),
            1,
            "the file did not exist before"
        );

        let state = web.apply(&settings).await.unwrap();
        assert_eq!(state["apid"], json!("none"));
        assert_eq!(calls(&web, "restart apid.service"), 1);

        settings.access.web.https_enabled = true;
        let state = web.apply(&settings).await.unwrap();
        assert_eq!(state["apid"], json!("restarted"));
        assert_eq!(calls(&web, "restart apid.service"), 2);
        assert_eq!(calls(&web, "start apid.service"), 0);
    }

    /// `off` stops a running apid and starts none; `local` runs it.
    #[tokio::test]
    async fn off_stops_apid_and_local_runs_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.access.web.mode = WebMode::Off;
        let running = reconciler(&dir, "active");
        assert_eq!(
            running.apply(&settings).await.unwrap()["apid"],
            json!("stopped")
        );
        assert_eq!(calls(&running, "stop apid.service"), 1);

        let stopped = reconciler(&dir, "inactive");
        assert_eq!(
            stopped.apply(&settings).await.unwrap()["apid"],
            json!("none")
        );
        assert_eq!(calls(&stopped, "start apid.service"), 0);

        settings.access.web.mode = WebMode::Local;
        let state = stopped.apply(&settings).await.unwrap();
        assert_eq!(state["apid"], json!("started"));
        assert_eq!(state["mode"], json!("local"));
    }

    /// An apid systemd gave up on is cleared before it is started again.
    #[tokio::test]
    async fn a_failed_apid_is_reset_then_started() {
        let dir = tempfile::tempdir().unwrap();
        let web = reconciler(&dir, "failed");
        assert_eq!(
            web.apply(&Settings::default()).await.unwrap()["apid"],
            json!("started")
        );
        assert_eq!(calls(&web, "reset-failed apid.service"), 1);
    }
}
