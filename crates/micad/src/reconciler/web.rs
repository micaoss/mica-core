//! Web reconciler: where apid listens.
//!
//! Renders `access.web` into the file apid reads at start and restarts a
//! running apid when it changed. On a boot micad renders it before apid starts
//! (apid is ordered after micad's readiness), so nothing is restarted then.

use std::path::PathBuf;

use anyhow::{Context, Result};
use micad_settings::{APID_LISTENERS_PATH, Settings};
use serde_json::json;

use super::Reconciler;
use super::systemd::{UnitControl, is_active};
use crate::fswrite::write_config_if_changed;

/// The unit apid runs as.
const APID_UNIT: &str = "apid.service";
/// World-readable, owner-writable: it names ports, nothing secret.
const LISTENERS_MODE: u32 = 0o644;

/// Reconciler for the `access.web` settings subtree.
pub struct WebReconciler<C: UnitControl> {
    path: PathBuf,
    control: C,
}

impl<C: UnitControl> WebReconciler<C> {
    /// Render to `path` and restart apid through `control`.
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
        // write is an apply task, and the restart moves the console to where
        // the operator was just told it now is.
        let restarted = changed && is_active(&self.control.active_state(APID_UNIT).await?);
        if restarted {
            self.control.restart(APID_UNIT).await?;
        }
        Ok(json!({
            "httpPort": web.http_port,
            "httpsEnabled": web.https_enabled,
            "httpsPort": web.https_port,
            "restarted": restarted,
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
            MockUnitControl::new(active, "enabled"),
        )
    }

    /// A boot: the file is rendered, and apid, not yet started, is left alone.
    #[tokio::test]
    async fn the_first_render_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let web = reconciler(&dir, "inactive");
        let state = web.apply(&Settings::default()).await.unwrap();
        assert_eq!(state["restarted"], json!(false));
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("apid.json")).unwrap())
                .unwrap();
        assert_eq!(
            file,
            json!({ "httpPort": 8080, "httpsEnabled": false, "httpsPort": 8443 })
        );
        assert!(!web.control.calls().iter().any(|c| c.starts_with("restart")));
    }

    /// A running apid is restarted when its listeners change, and only then.
    #[tokio::test]
    async fn a_running_apid_is_restarted_only_when_its_listeners_change() {
        let dir = tempfile::tempdir().unwrap();
        let web = reconciler(&dir, "active");
        let mut settings = Settings::default();
        web.apply(&settings).await.unwrap();
        let restarted = |web: &WebReconciler<MockUnitControl>| {
            web.control
                .calls()
                .iter()
                .filter(|call| call.as_str() == "restart apid.service")
                .count()
        };
        assert_eq!(restarted(&web), 1, "the file did not exist before");

        let state = web.apply(&settings).await.unwrap();
        assert_eq!(state["restarted"], json!(false));
        assert_eq!(restarted(&web), 1);

        settings.access.web.https_enabled = true;
        let state = web.apply(&settings).await.unwrap();
        assert_eq!(state["restarted"], json!(true));
        assert_eq!(restarted(&web), 2);
    }
}
