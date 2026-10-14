//! Settings writes and the apply queue that reconciles them.

use crate::apply_queue::{ApplyJob, ApplyQueue, TaskRecord};
use crate::reconciler::Reconciler;

use micad_settings::{DocumentRefusal, SettingsError};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use zbus::object_server::SignalEmitter;

use super::*;

impl MicadService {
    /// Validate and atomically persist `value` without waiting for a
    /// reconcile. The D-Bus method enqueues the apply after this returns.
    ///
    /// # Errors
    pub(super) async fn persist_setting(
        &self,
        path: &str,
        value: Value,
    ) -> Result<(), SettingsError> {
        // A path inside a feature the product does not carry is refused before
        // its value is read, so the answer is the same whatever was sent.
        if let Some(feature) = self.features.refuses(path) {
            return Err(SettingsError::NotServed {
                path: path.to_string(),
                feature,
            });
        }
        // **A refused document keeps its bytes unless this write is the one
        // that repairs it**. A refused subtree sits at
        // its schema default in the addressed tree, and `save` writes every
        // document out of that tree — so without this, an operator setting the
        // hostname would replace a poured `wifi.json` with the default nobody
        // chose, which is the silent revert the fail-closed rule forbids,
        // arriving one write later than the load.
        let preserve: Vec<String> = self
            .refusals
            .read()
            .await
            .iter()
            .filter(|refusal| !refusal_covers(refusal, path))
            .map(|refusal| refusal.document.clone())
            .collect();
        {
            let mut inner = self.inner.write().await;
            let mut candidate = inner.settings.clone();
            candidate.set(path, value)?;
            // A feature the product does not carry keeps whatever it holds: a
            // write around its subtree that would change it is refused too.
            for feature in micad_settings::Feature::ALL {
                if !self.features.has(feature)
                    && candidate.get(feature.subtree())? != inner.settings.get(feature.subtree())?
                {
                    return Err(SettingsError::NotServed {
                        path: path.to_string(),
                        feature,
                    });
                }
            }
            let preserve: Vec<&str> = preserve.iter().map(String::as_str).collect();
            self.store.preserving(&preserve).save(&candidate)?;
            inner.settings = candidate;
        }
        // The write landed, so the document it reached now holds what this
        // build writes and parses as this build reads: its refusal is over, and
        // the reconcilers it was gating run again on the apply that follows.
        let repaired = {
            let mut refusals = self.refusals.write().await;
            let before = refusals.len();
            refusals.retain(|refusal| !refusal_covers(refusal, path));
            before != refusals.len()
        };
        if repaired {
            self.publish_refusals().await;
        }
        Ok(())
    }

    /// Synchronous test hook for assertions whose subject is lock behaviour,
    /// not the queue. Production settings writes call [`Self::persist_setting`]
    /// and [`Self::enqueue_apply`] from `SetSettings`.
    #[cfg(test)]
    pub async fn write_setting(&self, path: &str, value: Value) -> Result<(), SettingsError> {
        let _apply = self.apply_lock.lock().await;
        self.persist_setting(path, value).await?;
        self.apply_subtree(path).await;
        Ok(())
    }

    /// Apply exactly the reconcilers whose declared subtree overlaps `path`.
    ///
    /// The caller holds [`Self::apply_lock`]. Settings are cloned under a read
    /// lock and each live-state result is recorded under a short write lock;
    /// no data lock is held while a reconciler waits on another process.
    pub(super) async fn apply_subtree(&self, path: &str) -> Vec<String> {
        // The agent learns the pairing code here, which is where the settings
        // that carry it have just changed. Anywhere else and the console would
        // be able to show one code while a peer is told another.
        if path.is_empty() || path.starts_with("bluetooth") {
            let settings = self.inner.read().await.settings.clone();
            self.agent.set_pin(self.pairing_pin(&settings)).await;
        }
        reconcile_subtree(&self.reconcilers, &self.inner, &self.refusals, path).await
    }

    /// Run every reconciler against the current settings, recording each
    /// result in the live-state tree. Errors are recorded, never propagated.
    pub async fn apply_all(&self) {
        let _apply = self.apply_lock.lock().await;
        self.apply_subtree("").await;
    }

    /// Queue one reconcile, publish its current record and ensure the one
    /// worker exists. The caller emits `SettingsChanged` separately because
    /// that signal describes persistence, while this lifecycle describes
    /// application.
    pub(super) async fn enqueue_apply(
        &self,
        emitter: &SignalEmitter<'_>,
        operation: &str,
        dot_path: &str,
        source: &str,
    ) -> TaskRecord {
        self.apply_queue.remember_emitter(emitter);
        self.ensure_apply_worker();
        let enqueued = self.apply_queue.enqueue(operation, dot_path, source).await;
        publish_task_transition(&self.apply_queue, &self.inner, &enqueued.record).await;
        if enqueued.created {
            self.apply_queue.wake();
        }
        enqueued.record
    }

    pub(super) fn ensure_apply_worker(&self) {
        if !self.apply_queue.claim_worker() {
            return;
        }
        tokio::spawn(run_apply_worker(
            Arc::clone(&self.apply_queue),
            Arc::clone(&self.inner),
            Arc::clone(&self.apply_lock),
            Arc::clone(&self.reconcilers),
            Arc::clone(&self.refusals),
        ));
    }

    /// Direct form retained only for unit tests whose subject is scoping or
    /// serialization. The production D-Bus member queues the apply and is
    /// exercised over a real bus by `micad/tests/bus.rs`.
    #[cfg(test)]
    pub(super) async fn set_transient_root_password(&self, password: &str) -> fdo::Result<()> {
        let _apply = self.apply_lock.lock().await;
        let shadow_path = self.shadow_path.clone();
        let password = password.to_string();
        tokio::task::spawn_blocking(move || {
            crate::transient::set_transient_root_password(&shadow_path, &password)
        })
        .await
        .map_err(|err| fdo::Error::Failed(format!("transient password task: {err}")))?
        .map_err(transient_to_fdo)?;
        self.apply_subtree("access.ssh").await;
        Ok(())
    }
}

/// Apply `path` against one immutable settings snapshot, recording each
/// reconciler result under a short data write lock. Returns failure messages
/// for the task outcome; one failing reconciler does not stop the rest.
pub(super) async fn reconcile_subtree(
    reconcilers: &[Box<dyn Reconciler>],
    inner: &RwLock<Inner>,
    refusals: &RwLock<Vec<DocumentRefusal>>,
    path: &str,
) -> Vec<String> {
    let settings = inner.read().await.settings.clone();
    let refused = refusals.read().await.clone();
    let mut failures = Vec::new();
    for reconciler in reconcilers {
        if !paths_overlap(path, reconciler.subtree()) {
            continue;
        }
        // **"Refuses its subsystem" is enforced here**. The reconciler whose settings come out of a document that did
        // not load is skipped rather than run against that document's schema
        // default, and the refusal takes the place of the applied state so the
        // live-state tree says why. Its neighbours are untouched: the loop
        // moves on, so a poured typo in one document costs exactly what that
        // document configures.
        if let Some(refusal) = refused
            .iter()
            .find(|refusal| refusal_covers(refusal, reconciler.subtree()))
        {
            tracing::warn!(
                reconciler = reconciler.name(),
                document = refusal.document,
                "reconciler skipped: the document that configures it did not load"
            );
            let mut inner = inner.write().await;
            if let Some(map) = inner.state.as_object_mut() {
                map.insert(
                    reconciler.name().to_string(),
                    serde_json::json!({ "refused": refusal.message }),
                );
            }
            continue;
        }
        let result = reconciler.apply(&settings).await;
        let mut inner = inner.write().await;
        if let Some(failure) = record(&mut inner.state, reconciler.name(), result) {
            failures.push(failure);
        }
    }
    failures
}

/// Whether `refusal` covers the dot-path `path`.
pub(super) fn refusal_covers(refusal: &DocumentRefusal, path: &str) -> bool {
    refusal
        .subtrees
        .iter()
        .any(|subtree| paths_overlap(path, subtree))
}

pub(super) async fn run_apply_worker(
    queue: Arc<ApplyQueue>,
    inner: Arc<RwLock<Inner>>,
    apply_lock: Arc<Mutex<()>>,
    reconcilers: Arc<Vec<Box<dyn Reconciler>>>,
    refusals: Arc<RwLock<Vec<DocumentRefusal>>>,
) {
    loop {
        let ApplyJob { id, dot_path } = queue.next().await;
        let Some(started) = queue.start(&id).await else {
            tracing::error!(task_id = id, "queued apply has no task record");
            continue;
        };
        publish_task_transition(&queue, &inner, &started).await;

        let failures = {
            let _apply = apply_lock.lock().await;
            reconcile_subtree(&reconcilers, &inner, &refusals, &dot_path).await
        };
        let (outcome, message) = if failures.is_empty() {
            ("succeeded", None)
        } else {
            ("failed", Some(failures.join("; ")))
        };
        if let Some(finished) = queue.finish(&id, outcome, message).await {
            publish_task_transition(&queue, &inner, &finished).await;
        }
    }
}

/// Keep the live-state `tasks` list and `TaskChanged` signal on the same
/// record. Signal failure is logged; subscribers then lapse and fall back to
/// `GetTask`, whose source of truth is the queue itself.
pub(super) async fn publish_task_transition(
    queue: &ApplyQueue,
    inner: &RwLock<Inner>,
    record: &TaskRecord,
) {
    let snapshot = queue.snapshot().await;
    let tasks = serde_json::to_value(snapshot).unwrap_or_else(|err| {
        tracing::error!(error = %err, "serialize apply task history");
        Value::Array(Vec::new())
    });
    let mut data = inner.write().await;
    if let Some(root) = data.state.as_object_mut() {
        root.insert("tasks".to_string(), tasks);
    }
    drop(data);

    let Some(emitter) = queue.emitter() else {
        return;
    };
    let json = match serde_json::to_string(record) {
        Ok(json) => json,
        Err(err) => {
            tracing::error!(error = %err, task_id = record.id, "serialize task transition");
            return;
        }
    };
    if let Err(err) = MicadService::task_changed(&emitter, &json).await {
        tracing::warn!(error = %err, task_id = record.id, "emit TaskChanged failed");
    }
}

/// Store a reconciler `result` in the live-state tree under `name`; a failure
/// is logged and recorded as `{"error": "..."}`.
pub(super) fn record(
    state: &mut Value,
    name: &str,
    result: anyhow::Result<Value>,
) -> Option<String> {
    let (entry, failure) = match result {
        Ok(value) => (value, None),
        Err(err) => {
            tracing::error!(reconciler = name, error = %err, "reconciler apply failed");
            let message = format!("{name}: {err}");
            (
                serde_json::json!({ "error": err.to_string() }),
                Some(message),
            )
        }
    };
    if let Some(map) = state.as_object_mut() {
        map.insert(name.to_string(), entry);
    }
    failure
}
