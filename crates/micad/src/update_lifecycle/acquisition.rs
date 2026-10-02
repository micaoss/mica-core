//! Checking for, fetching and importing an update.

use crate::deployment::Status;
use crate::update_codes::{self, CodedReason};
use crate::update_policy::{self, EffectivePolicy, Selection, Workspace};
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::*;

impl UpdateLifecycle {
    /// Start a metadata check (`sync` when a URL is configured, then
    /// `check`). Returns as soon as the work is handed to a background task;
    /// progress and the outcome land in `update.lifecycle`.
    pub async fn request_check(self: &Arc<Self>, sender: &str) -> Result<(), Refusal> {
        let policy = self.admit_check(sender).await?;
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.run_check(&policy).await;
            this.settle_check(result).await;
        });
        Ok(())
    }

    /// The same check, awaited to its outcome instead of spawned.
    ///
    /// Same admission, same subprocess, same recording — the difference is
    /// only that the caller learns what it found, which the automatic driver
    /// needs to decide its next step and an operator does not.
    pub async fn check_now(&self, sender: &str) -> Result<Settled<Available>, Refusal> {
        let policy = self.admit_check(sender).await?;
        let result = self.run_check(&policy).await;
        Ok(self.settle_check(result).await)
    }

    /// Admit a check: the policy refusal, the client, the busy slot. The
    /// policy it answers is the one the run must use, loaded once so the
    /// decision and the subprocess cannot read two different files.
    pub(super) async fn admit_check(&self, sender: &str) -> Result<EffectivePolicy, Refusal> {
        let loaded = self.policy.load();
        if let Some(refusal) = update_policy::check_refusal(&loaded) {
            self.record_refusal("check", &refusal).await;
            return Err(Refusal::Policy(refusal.text));
        }
        if let Some(reason) = self.client.unavailable() {
            self.record_refusal(
                "check",
                &CodedReason::new(update_codes::REFUSED_CLIENT_UNAVAILABLE, reason.clone()),
            )
            .await;
            return Err(Refusal::Unavailable(reason));
        }
        self.begin("checking").await?;
        tracing::info!(sender, "update check requested");
        Ok(loaded.policy)
    }

    /// Record a finished check and answer what it found.
    pub(super) async fn settle_check(
        &self,
        result: Result<CheckOutcome, Failure>,
    ) -> Settled<Available> {
        let mut machine = self.machine.lock().await;
        machine.operation = None;
        let settled = match result {
            Ok(CheckOutcome::Selected(available)) => {
                tracing::info!(
                    name = %available.deployment_id,
                    version = %available.version,
                    "update check selected a candidate"
                );
                machine.last_check = Some(now_rfc3339());
                machine.available = Some(available.clone());
                Settled::Done(available)
            }
            Ok(CheckOutcome::NoneCompatible) => {
                tracing::info!("update check found no compatible target");
                machine.last_check = Some(now_rfc3339());
                machine.available = None;
                Settled::NoneCompatible
            }
            Err(Failure::Unready(unready)) => {
                tracing::warn!(reason = %unready.reason(), "update workspace not ready; check not started");
                machine.unready = Some(unready.clone());
                Settled::Unready(unready)
            }
            Err(Failure::Error(failed)) => {
                tracing::warn!(
                    code = failed.code,
                    reason = failed.text,
                    "update check failed"
                );
                machine.last_check = Some(now_rfc3339());
                machine.failed = Some(failed.clone());
                Settled::Failed(failed)
            }
        };
        drop(machine);
        self.record_snapshot().await;
        settled
    }

    /// Start a descriptor fetch. Selection happens inside `mica-deploy fetch`
    /// itself, so a fetch does not require a prior check; on success the
    /// verified descriptor path is recorded and the state becomes `ready`.
    pub async fn request_fetch(self: &Arc<Self>, sender: &str) -> Result<(), Refusal> {
        let policy = self.admit_fetch(sender).await?;
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = this.run_fetch(&policy).await;
            this.settle_fetch(result).await;
        });
        Ok(())
    }

    /// Start an import, spawned like a fetch: the archive's objects are
    /// verified one by one and that is not a bus call's worth of time. The
    /// outcome lands in the update state the caller polls.
    pub async fn request_import(
        self: &Arc<Self>,
        sender: &str,
        archive: &Path,
    ) -> Result<(), Refusal> {
        let workspace = self.admit_import(sender).await?;
        let this = Arc::clone(self);
        let archive = archive.to_path_buf();
        tokio::spawn(async move {
            let result = this.run_import(&workspace, &archive).await;
            this.settle_fetch(result).await;
            // The upload is consumed: it has either been staged into the
            // workspace or refused, and either way the copy under `uploads/`
            // is a second megabyte-scale file on DATA that nothing reads.
            let _ = tokio::fs::remove_file(&archive).await;
        });
        Ok(())
    }

    /// The same import, awaited to its outcome instead of spawned; see
    /// [`Self::check_now`] for why the awaited form exists.
    ///
    /// Import an offline archive: the staging a fetch performs, from a file
    /// somebody carried here instead of from a server.
    ///
    /// **Not gated on the network policy.** A metered link, an absent source
    /// and an update mode of `off` all refuse a fetch and none of them has
    /// anything to say about a file already on the device: an operator who
    /// uploaded an archive has made the decision the policy exists to make.
    /// What still applies is the workspace probe, the client, the busy slot
    /// and -- inside `mica-deploy` -- every signature and product check an
    /// online acquisition runs, because it is the same code path.
    #[cfg(test)]
    pub async fn import_now(
        self: &Arc<Self>,
        sender: &str,
        archive: &Path,
    ) -> Result<Settled<String>, Refusal> {
        let workspace = self.admit_import(sender).await?;
        let result = self.run_import(&workspace, archive).await;
        Ok(self.settle_fetch(result).await)
    }

    /// Admit an import: the client and the busy slot, and nothing about the
    /// network.
    pub(super) async fn admit_import(&self, sender: &str) -> Result<Workspace, Refusal> {
        if let Some(reason) = self.client.unavailable() {
            self.record_refusal(
                "import",
                &CodedReason::new(update_codes::REFUSED_CLIENT_UNAVAILABLE, reason.clone()),
            )
            .await;
            return Err(Refusal::Unavailable(reason));
        }
        self.begin("importing").await?;
        tracing::info!(sender, "update import requested");
        Ok(self.policy.load().policy.workspace)
    }

    pub(super) async fn run_import(
        &self,
        workspace: &Workspace,
        archive: &Path,
    ) -> Result<FetchOutcome, Failure> {
        self.probe(workspace).await?;
        // The same read bound every acquisition carries. An import is the one
        // acquisition whose input a person chose, which is the reason to bound
        // it rather than a reason not to.
        let args = vec![
            "--max-bytes".to_string(),
            workspace.max_bytes.to_string(),
            "import".to_string(),
            archive.to_string_lossy().into_owned(),
        ];
        let output = self
            .client
            .run(&args, FETCH_TIMEOUT)
            .await
            .map_err(|error| {
                Failure::Error(CodedReason::new(
                    update_codes::CLIENT_SPAWN_FAILED,
                    format!("import: {error:#}"),
                ))
            })?;
        parse_fetch(&output, &self.verified_dir()).map_err(Failure::Error)
    }

    /// The same fetch, awaited to its outcome instead of spawned; see
    /// [`Self::check_now`] for why the awaited form exists.
    pub async fn fetch_now(&self, sender: &str) -> Result<Settled<String>, Refusal> {
        let policy = self.admit_fetch(sender).await?;
        let result = self.run_fetch(&policy).await;
        Ok(self.settle_fetch(result).await)
    }

    /// Admit a fetch: the policy refusal (which includes every check
    /// refusal, plus metered), the client, the busy slot.
    pub(super) async fn admit_fetch(&self, sender: &str) -> Result<EffectivePolicy, Refusal> {
        let loaded = self.policy.load();
        if let Some(refusal) = update_policy::fetch_refusal(&loaded) {
            self.record_refusal("fetch", &refusal).await;
            return Err(Refusal::Policy(refusal.text));
        }
        if let Some(reason) = self.client.unavailable() {
            self.record_refusal(
                "fetch",
                &CodedReason::new(update_codes::REFUSED_CLIENT_UNAVAILABLE, reason.clone()),
            )
            .await;
            return Err(Refusal::Unavailable(reason));
        }
        self.begin("downloading").await?;
        tracing::info!(sender, "update fetch requested");
        Ok(loaded.policy)
    }

    /// Record a finished fetch and answer what it staged.
    pub(super) async fn settle_fetch(
        &self,
        result: Result<FetchOutcome, Failure>,
    ) -> Settled<String> {
        let mut machine = self.machine.lock().await;
        machine.operation = None;
        let settled = match result {
            Ok(FetchOutcome::Staged(path)) => {
                tracing::info!(descriptor = %path, "update fetch staged a verified descriptor");
                machine.descriptor = Some(path.clone());
                Settled::Done(path)
            }
            Ok(FetchOutcome::NoneCompatible) => {
                tracing::info!("update fetch found no compatible target");
                machine.available = None;
                Settled::NoneCompatible
            }
            Err(Failure::Unready(unready)) => {
                tracing::warn!(reason = %unready.reason(), "update workspace not ready; fetch not started");
                machine.unready = Some(unready.clone());
                Settled::Unready(unready)
            }
            Err(Failure::Error(failed)) => {
                tracing::warn!(
                    code = failed.code,
                    reason = failed.text,
                    "update fetch failed"
                );
                machine.failed = Some(failed.clone());
                Settled::Failed(failed)
            }
        };
        drop(machine);
        self.record_snapshot().await;
        settled
    }

    /// The candidate the last check selected, if it selected one.
    pub async fn available(&self) -> Option<Available> {
        self.machine.lock().await.available.clone()
    }

    /// The verified descriptor path the last fetch staged, if one is staged.
    pub async fn staged_descriptor(&self) -> Option<String> {
        self.machine.lock().await.descriptor.clone()
    }

    /// Discard all bounded acquisition files under the native transaction lock.
    pub async fn discard_descriptor(&self, why: &str) {
        if self.machine.lock().await.descriptor.is_none() {
            return;
        }
        if let Err(refusal) = self.begin("discarding").await {
            tracing::warn!(reason = refusal.message(), "workspace discard refused");
            return;
        }
        let result = self.client.run(&["discard".into()], PROBE_TIMEOUT).await;
        let mut machine = self.machine.lock().await;
        machine.operation = None;
        machine.descriptor = None;
        machine.available = None;
        let error = match result {
            Ok(output) => client_json("discard", &output).err(),
            Err(error) => Some(CodedReason::new(
                update_codes::CLIENT_SPAWN_FAILED,
                format!("discard: {error:#}"),
            )),
        };
        machine.failed = error;
        drop(machine);
        self.record_snapshot_with(Some(CodedReason::new(
            update_codes::NOTE_DEPLOYMENT_DISCARDED,
            format!("staged deployment discarded: {why}"),
        )))
        .await;
    }

    /// Take the busy slot for `operation`, refusing when anything runs.
    pub(super) async fn begin(&self, operation: &'static str) -> Result<(), Refusal> {
        if self.installing.load(Ordering::Acquire) {
            return Err(Refusal::Busy(
                "an update install is running; query GetUpdateState and retry".to_string(),
            ));
        }
        let mut machine = self.machine.lock().await;
        if let Some(running) = machine.operation {
            return Err(Refusal::Busy(format!(
                "an update operation is already {running}; query GetUpdateState and retry"
            )));
        }
        machine.operation = Some(operation);
        machine.failed = None;
        machine.unready = None;
        drop(machine);
        self.record_snapshot().await;
        Ok(())
    }

    /// The readiness probe, before any acquisition: `mica-deploy
    /// probe` against the policy's budget. A passing probe records the
    /// workspace report; a failing one is the `update-unavailable` state.
    pub(super) async fn probe(&self, workspace: &Workspace) -> Result<(), Failure> {
        let args = vec![
            "--max-bytes".to_string(),
            workspace.max_bytes.to_string(),
            "probe".to_string(),
        ];
        let output = self.client.run(&args, PROBE_TIMEOUT).await.map_err(|err| {
            Failure::Error(CodedReason::new(
                update_codes::CLIENT_SPAWN_FAILED,
                format!("probe: {err:#}"),
            ))
        })?;
        match parse_probe(&output).map_err(Failure::Error)? {
            ProbeOutcome::Ready(report) => {
                self.machine.lock().await.workspace = Some(report);
                Ok(())
            }
            ProbeOutcome::Unready(unready) => Err(Failure::Unready(unready)),
        }
    }

    pub(super) async fn run_check(
        &self,
        policy: &EffectivePolicy,
    ) -> Result<CheckOutcome, Failure> {
        self.probe(&policy.workspace).await?;
        let args = acquisition_args("check", selection_of(policy)?, &policy.workspace)?;
        let output = self
            .client
            .run(&args, CHECK_TIMEOUT)
            .await
            .map_err(|error| {
                Failure::Error(CodedReason::new(
                    update_codes::CLIENT_SPAWN_FAILED,
                    format!("check: {error:#}"),
                ))
            })?;
        parse_check(&output).map_err(Failure::Error)
    }

    pub(super) async fn run_fetch(
        &self,
        policy: &EffectivePolicy,
    ) -> Result<FetchOutcome, Failure> {
        self.probe(&policy.workspace).await?;
        let args = acquisition_args("fetch", selection_of(policy)?, &policy.workspace)?;
        let output = self
            .client
            .run(&args, FETCH_TIMEOUT)
            .await
            .map_err(|error| {
                Failure::Error(CodedReason::new(
                    update_codes::CLIENT_SPAWN_FAILED,
                    format!("fetch: {error:#}"),
                ))
            })?;
        parse_fetch(&output, &self.verified_dir()).map_err(Failure::Error)
    }

    /// The status and the phase are derived from the same native observation.
    pub async fn refresh(&self, status: &Status) {
        self.machine.lock().await.boot_phase = Some(status.phase());
        self.record_snapshot().await;
    }
}

pub(super) fn acquisition_args(
    verb: &str,
    selection: &Selection,
    workspace: &Workspace,
) -> Result<Vec<String>, Failure> {
    let source = selection.url.as_ref().ok_or_else(|| {
        Failure::Error(CodedReason::new(
            update_codes::NO_SOURCE_CONFIGURED,
            "no update source configured",
        ))
    })?;
    Ok(vec![
        "--max-bytes".into(),
        workspace.max_bytes.to_string(),
        verb.into(),
        "--source".into(),
        source.clone(),
    ])
}
