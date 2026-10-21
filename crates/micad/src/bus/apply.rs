//! Settings writes and the apply queue that reconciles them.

use crate::apply_queue::{ApplyJob, ApplyQueue, TaskRecord};
use crate::reconciler::Reconciler;

use micad_settings::{DocumentRefusal, IfaceSettings, SettingsError, Store};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use zbus::object_server::SignalEmitter;

use super::*;

/// The network settings subtree: interfaces by name.
type NetworkMap = std::collections::BTreeMap<String, IfaceSettings>;

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
        self.persist_settings(&[(path.to_string(), value)]).await
    }

    /// Validate and atomically persist every `(path, value)` in `writes`, in
    /// order, as one write: all of them land or none does.
    ///
    /// # Errors
    pub(super) async fn persist_settings(
        &self,
        writes: &[(String, Value)],
    ) -> Result<(), SettingsError> {
        // The path a refusal of the write as a whole names.
        let path = writes.first().map_or("", |(path, _)| path.as_str());
        // A path inside a feature the product does not carry is refused before
        // its value is read, so the answer is the same whatever was sent.
        for (path, _) in writes {
            if let Some(feature) = self.features.refuses(path) {
                return Err(SettingsError::NotServed {
                    path: path.clone(),
                    feature,
                });
            }
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
            .filter(|refusal| !writes.iter().any(|(path, _)| refusal_covers(refusal, path)))
            .map(|refusal| refusal.document.clone())
            .collect();
        // Run before the tree's lock is taken, since it runs a process.
        let checked = self.check_written_projects(writes).await?;
        let sealed = {
            let mut inner = self.inner.write().await;
            let mut candidate = inner.settings.clone();
            for (path, value) in writes {
                candidate.set(path, value.clone())?;
            }
            self.user_units
                .check_written(&inner.settings.services, &candidate.services)
                .map_err(|message| SettingsError::Validation {
                    path: path.to_string(),
                    message,
                })?;
            if changed_projects(&inner.settings, &candidate).any(|(name, written)| {
                checked.get(name).map(|(text, dir)| (text, dir.as_deref())) != Some(written)
            }) {
                return Err(SettingsError::Validation {
                    path: path.to_string(),
                    message:
                        "the container projects changed while this write was checked; send it again"
                            .into(),
                });
            }
            // The claim seals the pairing code. Before it the code is the
            // default one, which a setup over Bluetooth needs; once the
            // device has an administrator and Bluetooth is on it is six random
            // digits, written by the write that made both true: the claim, or
            // switching Bluetooth on after it. A declared code is somebody's
            // and stays.
            let sealable = |settings: &micad_settings::Settings| {
                settings.access.web_admin.is_some() && settings.bluetooth.enabled
            };
            let sealed = (!sealable(&inner.settings)
                && sealable(&candidate)
                && candidate.bluetooth.pairing_pin() == micad_settings::DEFAULT_PAIRING_PIN
                && self.features.has(micad_settings::Feature::Bluetooth)
                && !preserve
                    .iter()
                    .any(|document| document == micad_settings::BLUETOOTH_DOCUMENT))
            .then(crate::bluetooth::random_pairing_pin)
            .flatten();
            if let Some(pin) = &sealed {
                candidate.bluetooth.pin = Some(pin.clone());
            }
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
            sealed
        };
        if let Some(pin) = sealed {
            tracing::info!("the default Bluetooth pairing code was replaced");
            self.agent.set_pin(pin).await;
        }
        // The write landed, so the document it reached now holds what this
        // build writes and parses as this build reads: its refusal is over, and
        // the reconcilers it was gating run again on the apply that follows.
        let repaired = {
            let mut refusals = self.refusals.write().await;
            let before = refusals.len();
            refusals
                .retain(|refusal| !writes.iter().any(|(path, _)| refusal_covers(refusal, path)));
            before != refusals.len()
        };
        if repaired {
            self.publish_refusals().await;
        }
        Ok(())
    }

    /// Hold every project this write adds or changes to mica-containerd's
    /// check, and answer the texts that passed. A write that does not parse
    /// is left to the tree to refuse.
    async fn check_written_projects(
        &self,
        writes: &[(String, Value)],
    ) -> Result<std::collections::BTreeMap<String, (String, Option<String>)>, SettingsError> {
        let path = writes.first().map_or("", |(path, _)| path.as_str());
        let current = self.inner.read().await.settings.clone();
        let mut candidate = current.clone();
        for (path, value) in writes {
            if candidate.set(path, value.clone()).is_err() {
                return Ok(std::collections::BTreeMap::new());
            }
        }
        // On docker nothing is declared to micad; taking projects away is
        // still a write, since it declares none.
        let (before, after) = (&current.container, &candidate.container);
        if self.container_engine == micad_settings::ContainerEngine::Docker
            && (before.projects != after.projects || before.project_dirs != after.project_dirs)
            && !(after.projects.is_empty() && after.project_dirs.is_empty())
        {
            return Err(SettingsError::Validation {
                path: path.to_string(),
                message: crate::docker::USE_DOCKER_COMPOSE.into(),
            });
        }
        let mut checked = std::collections::BTreeMap::new();
        for (name, (text, dir)) in changed_projects(&current, &candidate) {
            let refused = match self.compose.check(name, text, dir).await {
                Ok(outcome) => crate::compose::refusal(name, &outcome),
                Err(err) => Some(format!(
                    "container project {name:?} cannot be checked: {err}"
                )),
            };
            if let Some(message) = refused {
                return Err(SettingsError::Validation {
                    path: path.to_string(),
                    message,
                });
            }
            checked.insert(name.clone(), (text.clone(), dir.map(str::to_string)));
        }
        Ok(checked)
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
    #[cfg(test)]
    pub async fn apply_all(&self) {
        let _apply = self.apply_lock.lock().await;
        self.apply_subtree("").await;
    }

    /// The apply micad starts with: every reconciler but the [`DEFERRED`]
    /// ones, whose first apply the returned [`DeferredApply`] runs once micad
    /// is on the bus.
    pub async fn apply_startup(&self) -> DeferredApply {
        {
            let _apply = self.apply_lock.lock().await;
            let settings = self.inner.read().await.settings.clone();
            self.agent.set_pin(self.pairing_pin(&settings)).await;
            reconcile_matching(&self.reconcilers, &self.inner, &self.refusals, "", |name| {
                !DEFERRED.contains(&name)
            })
            .await;
        }
        DeferredApply {
            reconcilers: Arc::clone(&self.reconcilers),
            inner: Arc::clone(&self.inner),
            refusals: Arc::clone(&self.refusals),
            apply_lock: Arc::clone(&self.apply_lock),
            pause: DEFERRED_RETRY_PAUSE,
            retry_for: DEFERRED_RETRY_FOR,
        }
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
            self.store.clone(),
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

/// The reconcilers that start or restart another service. Their apply can wait
/// on that service: the container reconciler waits for mica-containerd, and on
/// OpenRC `rc-service start` waits for the service and what it needs -- the
/// MQTT units need micad itself, mica-ntpd needs the network. Their startup
/// apply runs after micad claims its bus name, so micad's readiness waits on
/// none of them; the ones that run before it only render files and bus calls.
pub(super) const DEFERRED: [&str; 9] = [
    "dns",
    "sshd",
    "wifiClient",
    "wifiAp",
    "container",
    "mqtt",
    "time",
    "services",
    "bluetooth",
];
/// Pause between two attempts of a failed deferred apply.
const DEFERRED_RETRY_PAUSE: Duration = Duration::from_secs(10);
/// How long a failed deferred apply is retried for. A settings write applies
/// the reconciler again whenever it is made.
const DEFERRED_RETRY_FOR: Duration = Duration::from_secs(300);

/// The startup apply of the [`DEFERRED`] reconcilers, taken from the service
/// before it moves onto the bus.
pub struct DeferredApply {
    reconcilers: Arc<Vec<Box<dyn Reconciler>>>,
    inner: Arc<RwLock<Inner>>,
    refusals: Arc<RwLock<Vec<DocumentRefusal>>>,
    apply_lock: Arc<Mutex<()>>,
    pause: Duration,
    retry_for: Duration,
}

impl DeferredApply {
    /// Shorter waits, for tests.
    #[cfg(test)]
    #[must_use]
    pub(super) fn with_retry(mut self, pause: Duration, retry_for: Duration) -> Self {
        self.pause = pause;
        self.retry_for = retry_for;
        self
    }

    /// Apply the deferred reconcilers, recording each outcome in the
    /// live-state tree as every apply does, and again after a pause while one
    /// fails -- a daemon that was not ready yet -- for at most the retry time.
    pub async fn run(self) {
        let deadline = tokio::time::Instant::now() + self.retry_for;
        loop {
            let failures = {
                let _apply = self.apply_lock.lock().await;
                reconcile_matching(&self.reconcilers, &self.inner, &self.refusals, "", |name| {
                    DEFERRED.contains(&name)
                })
                .await
            };
            if failures.is_empty() || tokio::time::Instant::now() + self.pause > deadline {
                return;
            }
            tokio::time::sleep(self.pause).await;
        }
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
    reconcile_matching(reconcilers, inner, refusals, path, |_| true).await
}

/// [`reconcile_subtree`] over the reconcilers whose name `keep` accepts.
async fn reconcile_matching(
    reconcilers: &[Box<dyn Reconciler>],
    inner: &RwLock<Inner>,
    refusals: &RwLock<Vec<DocumentRefusal>>,
    path: &str,
    keep: impl Fn(&str) -> bool,
) -> Vec<String> {
    let settings = inner.read().await.settings.clone();
    let refused = refusals.read().await.clone();
    let mut failures = Vec::new();
    for reconciler in reconcilers {
        let watched =
            std::iter::once(reconciler.subtree()).chain(reconciler.reads().iter().copied());
        if !watched
            .into_iter()
            .any(|subtree| paths_overlap(path, subtree))
            || !keep(reconciler.name())
        {
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
        if reconciler.subtree() == "network" && result.is_ok() {
            inner.applied_network = Some(settings.network.clone());
        }
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
    store: Store,
) {
    loop {
        let ApplyJob { id, dot_path } = queue.next().await;
        let Some(started) = queue.start(&id).await else {
            tracing::error!(task_id = id, "queued apply has no task record");
            continue;
        };
        publish_task_transition(&queue, &inner, &started).await;

        let (failures, interfaces) = {
            let _apply = apply_lock.lock().await;
            let (applied, attempted) = {
                let data = inner.read().await;
                (data.applied_network.clone(), data.settings.network.clone())
            };
            let failures = reconcile_subtree(&reconcilers, &inner, &refusals, &dot_path).await;
            if !paths_overlap(&dot_path, "network") {
                (failures, Vec::new())
            } else {
                let interfaces = changed_interfaces(applied.as_ref(), &attempted);
                if let Some(applied) = applied {
                    restore_network(&queue, &inner, &refusals, &store, &attempted, applied).await;
                }
                (failures, interfaces)
            }
        };
        let (outcome, message) = if failures.is_empty() {
            ("succeeded", None)
        } else {
            ("failed", Some(failures.join("; ")))
        };
        if let Some(finished) = queue.finish(&id, outcome, message, interfaces).await {
            publish_task_transition(&queue, &inner, &finished).await;
        }
    }
}

/// The interfaces `attempted` adds, changes or removes against `applied`.
fn changed_interfaces(applied: Option<&NetworkMap>, attempted: &NetworkMap) -> Vec<String> {
    let empty = NetworkMap::new();
    let applied = applied.unwrap_or(&empty);
    applied
        .keys()
        .chain(attempted.keys())
        .filter(|name| applied.get(*name) != attempted.get(*name))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .cloned()
        .collect()
}

/// After a network apply the reconciler rolled back, store `applied` again
/// in place of the `attempted` map it could not bring up, so the stored map
/// is the one running. A write that landed since is newer than both, and
/// stays.
async fn restore_network(
    queue: &ApplyQueue,
    inner: &RwLock<Inner>,
    refusals: &RwLock<Vec<DocumentRefusal>>,
    store: &Store,
    attempted: &NetworkMap,
    applied: NetworkMap,
) {
    let preserve: Vec<String> = refusals
        .read()
        .await
        .iter()
        .map(|refusal| refusal.document.clone())
        .collect();
    let value = {
        let mut data = inner.write().await;
        let rolled_back = data
            .state
            .pointer("/network/rolledBack")
            .and_then(Value::as_bool)
            == Some(true);
        if !rolled_back || data.settings.network != *attempted {
            return;
        }
        let mut candidate = data.settings.clone();
        candidate.network = applied;
        let preserve: Vec<&str> = preserve.iter().map(String::as_str).collect();
        if let Err(error) = store.preserving(&preserve).save(&candidate) {
            tracing::error!(%error, "store the restored network map");
            return;
        }
        let value = serde_json::to_string(&candidate.network);
        data.settings = candidate;
        value
    };
    let (Some(emitter), Ok(value)) = (queue.emitter(), value) else {
        return;
    };
    if let Err(error) = MicadService::settings_changed(&emitter, "network", &value).await {
        tracing::warn!(%error, "emit SettingsChanged for the restored network map");
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
            let mut entry = serde_json::json!({ "error": err.to_string() });
            if err
                .downcast_ref::<crate::reconciler::network::RolledBack>()
                .is_some()
            {
                entry["rolledBack"] = Value::Bool(true);
            }
            (entry, Some(message))
        }
    };
    if let Some(map) = state.as_object_mut() {
        map.insert(name.to_string(), entry);
    }
    failure
}

/// The projects `next` adds or changes against `current`, a text or its
/// directory, each with the text and directory it has in `next`.
fn changed_projects<'a>(
    current: &'a micad_settings::Settings,
    next: &'a micad_settings::Settings,
) -> impl Iterator<Item = (&'a String, (&'a String, Option<&'a str>))> {
    let written = |settings: &'a micad_settings::Settings, name: &str| {
        settings.container.projects.get(name).map(|text| {
            (
                text,
                settings
                    .container
                    .project_dirs
                    .get(name)
                    .map(String::as_str),
            )
        })
    };
    next.container.projects.keys().filter_map(move |name| {
        let now = written(next, name)?;
        (written(current, name) != Some(now)).then_some((name, now))
    })
}
