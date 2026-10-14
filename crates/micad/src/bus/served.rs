//! The daemon as the automatic update driver reaches it.

use crate::time_status::ClockTrust;
use crate::update_auto::{self, AutoRoutes, UpdateFacts};
use crate::update_lifecycle::{Available, Refusal, Settled, UpdateLifecycle};
use crate::update_policy::LoadedPolicy;
use micad_settings::{ACTOR_POLICY, REQUESTED, append_audit_line, audit_line, audit_ring_dir};
use serde_json::Value;
use std::sync::Arc;
use zbus::fdo;
use zbus::object_server::InterfaceRef;

use super::*;

/// The four service routes the automatic driver reaches, and the ONE hop a
/// test cannot build.
#[async_trait::async_trait]
pub trait ServedDaemon: Send + Sync {
    /// `GetUpdateState`'s own document, read exactly as an operator polling
    /// the API reads it.
    async fn update_state(&self) -> fdo::Result<String>;

    /// The `InstallUpdate` route, with every gate it answers an operator with.
    async fn install(&self, sender: &str, descriptor: &str) -> fdo::Result<()>;

    /// The `Reboot` route, honouring the safe-to-reboot gate.
    async fn reboot(&self, sender: &str) -> fdo::Result<()>;

    /// The clock-trust evidence.
    async fn clock_trust(&self) -> ClockTrust;
}

#[async_trait::async_trait]
impl ServedDaemon for InterfaceRef<MicadService> {
    async fn update_state(&self) -> fdo::Result<String> {
        self.get().await.refresh_update_state().await
    }

    async fn install(&self, sender: &str, descriptor: &str) -> fdo::Result<()> {
        self.get().await.request_install(sender, descriptor).await
    }

    async fn reboot(&self, sender: &str) -> fdo::Result<()> {
        self.get().await.request_reboot(sender).await
    }

    async fn clock_trust(&self) -> ClockTrust {
        self.get().await.clock_trust().await
    }
}

/// The automatic driver's window onto the daemon.
pub struct BusRoutes<S> {
    pub(super) lifecycle: Arc<UpdateLifecycle>,
    pub(super) service: S,
}

impl<S: ServedDaemon> BusRoutes<S> {
    pub fn new(lifecycle: Arc<UpdateLifecycle>, service: S) -> Self {
        Self { lifecycle, service }
    }

    /// `GetUpdateState`, parsed. A query that did not answer is `None`, which
    /// the driver reads as "ask again next tick" and never as "nothing is
    /// pending".
    pub(super) async fn update_state(&self) -> Option<Value> {
        let rendered = self
            .service
            .update_state()
            .await
            .map_err(|err| {
                tracing::debug!(error = %err, "automatic driver could not read the update state");
            })
            .ok()?;
        serde_json::from_str(&rendered).ok()
    }
}

#[async_trait::async_trait]
impl<S: ServedDaemon + 'static> AutoRoutes for BusRoutes<S> {
    fn policy(&self) -> LoadedPolicy {
        self.lifecycle.policy().load()
    }

    async fn check(&self, sender: &str) -> Result<Settled<Available>, Refusal> {
        self.lifecycle.check_now(sender).await
    }

    async fn fetch(&self, sender: &str) -> Result<Settled<String>, Refusal> {
        self.lifecycle.fetch_now(sender).await
    }

    async fn available(&self) -> Option<Available> {
        self.lifecycle.available().await
    }

    async fn staged(&self) -> Option<String> {
        self.lifecycle.staged_descriptor().await
    }

    async fn discard_staged(&self, why: &str) {
        self.lifecycle.discard_descriptor(why).await;
    }

    async fn facts(&self) -> Option<UpdateFacts> {
        let entry = self.update_state().await?;
        Some(UpdateFacts {
            reboot_pending: entry
                .pointer("/state/candidate")
                .is_some_and(Value::is_string)
                || entry.pointer("/lifecycle/state").and_then(Value::as_str)
                    == Some("reboot-required"),
            install_status: entry
                .pointer("/install/status")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn install(&self, sender: &str, descriptor: &str) -> Result<(), String> {
        self.service
            .install(sender, descriptor)
            .await
            .map_err(|err| err.to_string())
    }

    async fn reboot(&self, sender: &str) -> Result<(), String> {
        self.service
            .reboot(sender)
            .await
            .map_err(|err| err.to_string())
    }

    async fn clock(&self) -> ClockTrust {
        self.service.clock_trust().await
    }

    async fn audit(&self, event: &str) {
        record_policy_action(&audit_ring_dir(), event);
    }

    async fn defer(&self, reason: &str, detail: &str) {
        self.lifecycle.defer(reason, detail).await;
    }

    async fn resume(&self, only: Option<&str>) {
        self.lifecycle.resume(only).await;
    }
}

/// One audit line for an action the update policy took on its own.
pub(super) fn record_policy_action(dir: &std::path::Path, event: &str) {
    tracing::info!(
        target: "audit",
        event,
        outcome = REQUESTED,
        source = update_auto::SENDER,
        actor = ACTOR_POLICY,
        "audit event"
    );
    if let Err(err) = std::fs::create_dir_all(dir).and_then(|()| {
        append_audit_line(
            dir,
            &audit_line(event, REQUESTED, update_auto::SENDER, ACTOR_POLICY),
        )
    }) {
        tracing::warn!(
            error = %err,
            dir = %dir.display(),
            "the automatic update audit line could not be written"
        );
    }
}
