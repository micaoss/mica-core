//! Reboot, power-off, update installation and deployment actions.

use crate::deployment;
use crate::time_status::{self, ClockTrust};
use crate::update_codes;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use zbus::fdo;

use super::*;

impl MicadService {
    /// Log a power request from `sender` and record it in the live-state tree
    /// under `power`, with `update_warning` — an unconfirmed-deployment warning, when
    /// there is one — recorded beside it (absent key when there is none, the
    /// same convention the settings tree uses for optional values).
    pub(super) async fn note_power_request(
        &self,
        action: &str,
        sender: &str,
        update_warning: Option<String>,
    ) {
        tracing::warn!(action, sender, "power action requested");
        let mut entry = serde_json::Map::new();
        entry.insert("last_action".into(), Value::String(action.to_string()));
        entry.insert("requested_by".into(), Value::String(sender.to_string()));
        if let Some(warning) = update_warning {
            entry.insert("update_warning".into(), Value::String(warning));
        }
        let mut inner = self.inner.write().await;
        if let Some(root) = inner.state.as_object_mut() {
            root.insert("power".to_string(), Value::Object(entry));
        }
        drop(inner);
    }

    /// Read authenticated deployment evidence without delaying unrelated power or information calls.
    pub(super) async fn deployment_evidence(&self) -> Option<deployment::Status> {
        match tokio::time::timeout(Duration::from_secs(2), self.deployments.status()).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(error)) => {
                tracing::debug!(%error, "deployment status unavailable");
                None
            }
            Err(_) => {
                tracing::warn!("deployment query timed out");
                None
            }
        }
    }

    pub(super) async fn reboot_update_warning(&self) -> Option<String> {
        let status = self.deployment_evidence().await?;
        let (phase, reason) = status.phase();
        matches!(phase, "reboot-required" | "validating").then_some(reason)
    }

    /// Reboot the machine on behalf of `sender`.
    pub async fn request_reboot(&self, sender: &str) -> fdo::Result<()> {
        // The safe-to-reboot interlock, and the one REFUSAL on this path.
        // Distinct from the unconfirmed-deployment warning below, which stays a
        // warning: booting a fresh deployment is what an updating operator wants,
        // while rebooting through an application's declared blocking work —
        // or through an install mid-write — is what nobody wants. The gate
        // opens by the reporter clearing its status, the install finishing,
        // or a bounded audited override (`SetRebootOverride`).
        if let Some(refusal) = self.update.reboot_refusal().await {
            tracing::warn!(sender, refusal, "reboot refused by the safe-to-reboot gate");
            return Err(fdo::Error::AccessDenied(refusal));
        }
        let warning = self.reboot_update_warning().await;
        if let Some(warning) = &warning {
            tracing::warn!(warning, "rebooting with an unconfirmed deployment");
        }
        self.note_power_request("reboot", sender, warning).await;
        self.power
            .reboot()
            .await
            .map_err(|err| fdo::Error::Failed(format!("reboot: {err}")))
    }

    /// Power the machine off on behalf of `sender`.
    ///
    /// No unconfirmed-deployment warning here, deliberately: a power-off does not
    /// boot anything, so it spends no boot attempt. The attempt is spent by
    /// whatever powers the machine back ON, which is not an event micad can
    /// see, let alone warn about.
    pub async fn request_power_off(&self, sender: &str) -> fdo::Result<()> {
        self.note_power_request("power_off", sender, None).await;
        self.power
            .power_off()
            .await
            .map_err(|err| fdo::Error::Failed(format!("power off: {err}")))
    }

    /// Admit a verified descriptor and record the asynchronous native installation.
    pub async fn request_install(&self, sender: &str, descriptor_path: &str) -> fdo::Result<()> {
        if let Some(refusal) = self.update.install_refusal() {
            return Err(fdo::Error::AccessDenied(refusal));
        }
        let descriptor = PathBuf::from(descriptor_path);
        self.update
            .installable(&descriptor)
            .map_err(fdo::Error::InvalidArgs)?;
        let id = deployment::staged_id(&descriptor)
            .ok_or_else(|| fdo::Error::InvalidArgs("the descriptor names no deployment".into()))?
            .0
            .to_owned();
        if self
            .installing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(fdo::Error::Failed(
                "an update install is already running".into(),
            ));
        }
        tracing::warn!(deployment_id = id, sender, "deployment install requested");
        update_entry(&mut self.inner.write().await.state).insert(
            "install".into(),
            serde_json::json!({"status":"running","deploymentId":id,"requested_by":sender}),
        );
        let client = Arc::clone(&self.deployments);
        let inner = Arc::clone(&self.inner);
        let installing = Arc::clone(&self.installing);
        let lifecycle = Arc::clone(&self.update);
        let sender = sender.to_owned();
        tokio::spawn(async move {
            let outcome = match client.install(&descriptor).await {
                Ok(()) => {
                    serde_json::json!({"status":"done","deploymentId":id,"requested_by":sender})
                }
                Err(error) => {
                    tracing::error!(deployment_id = id, %error, "deployment install failed");
                    serde_json::json!({"status":"failed","deploymentId":id,"requested_by":sender,
                        "error":format!("{error:#}"),"error_code":update_codes::CLIENT_EXIT_FAILURE})
                }
            };
            let status = client.status().await.ok();
            let mut guard = inner.write().await;
            let entry = update_entry(&mut guard.state);
            entry.insert("install".into(), outcome);
            if let Some(status) = &status
                && let Err(error) = status.merge_into(entry)
            {
                tracing::error!(%error, "deployment status encoding failed");
            }
            drop(guard);
            installing.store(false, Ordering::Release);
            if let Some(status) = status {
                lifecycle.installed(&status).await;
            }
        });
        Ok(())
    }

    /// Refresh every deployment fact from the authenticated native backend.
    pub async fn refresh_update_state(&self) -> fdo::Result<String> {
        let status = self
            .deployments
            .status()
            .await
            .map_err(|error| fdo::Error::Failed(format!("query deployments: {error:#}")))?;
        self.update.refresh(&status).await;
        let mut inner = self.inner.write().await;
        let entry = update_entry(&mut inner.state);
        status
            .merge_into(entry)
            .map_err(|error| fdo::Error::Failed(error.to_string()))?;
        Ok(Value::Object(entry.clone()).to_string())
    }

    /// Explicit operator action; automatic confirmation belongs to the boot health gate.
    pub async fn request_deployment_action(
        &self,
        sender: &str,
        action: &str,
        id: &str,
    ) -> fdo::Result<()> {
        if !deployment::valid_id(id) || !matches!(action, "confirm" | "reject" | "rollback") {
            return Err(fdo::Error::InvalidArgs(
                "invalid deployment action or ID".into(),
            ));
        }
        if self.installing.load(Ordering::Acquire) {
            return Err(fdo::Error::Failed("an update install is running".into()));
        }
        let status = self
            .deployments
            .status()
            .await
            .map_err(|error| fdo::Error::Failed(error.to_string()))?;
        if action != "reject" && status.boot.deployment_id != id {
            return Err(fdo::Error::InvalidArgs(
                "action must name the running deployment".into(),
            ));
        }
        tracing::warn!(
            sender,
            action,
            deployment_id = id,
            "manual deployment action requested"
        );
        match action {
            "confirm" => self.deployments.confirm().await,
            "reject" => self.deployments.reject(id).await,
            "rollback" => self.deployments.rollback().await,
            _ => unreachable!("validated action"),
        }
        .map_err(|error| fdo::Error::Failed(format!("{action}: {error:#}")))?;
        update_entry(&mut self.inner.write().await.state).insert(
            "last_action".into(),
            serde_json::json!({"action":action,"deploymentId":id,"requested_by":sender}),
        );
        self.refresh_update_state().await?;
        Ok(())
    }

    /// The clock-trust evidence the automatic-install predicate
    /// reads: the classification, and the saved
    /// floor.
    pub async fn clock_trust(&self) -> ClockTrust {
        let status = self
            .time_status
            .observe()
            .await
            .ok()
            .map(|evidence| time_status::classify(&evidence));
        time_status::observed_clock_trust(status)
    }
}
