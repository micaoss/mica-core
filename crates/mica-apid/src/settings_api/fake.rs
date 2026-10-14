//! An in-memory settings backend for tests.

use serde_json::Value;

use super::*;
use crate::task_registry::TaskRecord;

/// A rendezvous armed over [`FakeSettings::hold_access_reads`]: the first
/// `parties` reads of the `access` subtree wait for each other before they are
/// answered.
///
/// It exists for the concurrent-claim test, which needs both claimants to have
/// taken the read half of the claim's check-then-act before either takes the
/// write half. **It widens a window it does not create** — the window is the
/// argon2id hash and the bus round trips the route runs between its read and
/// its write.
///
/// The wait is bounded, and the bound is load-bearing rather than defensive:
/// once the claim is one step the second read cannot happen until the first
/// request has finished, so the first read waits the bound out and then
/// proceeds. An unbounded barrier would hang there instead.
#[cfg(test)]
pub(super) struct AccessHold {
    pub(super) barrier: tokio::sync::Barrier,
    pub(super) parties: usize,
    pub(super) timeout: std::time::Duration,
}

/// In-memory [`SettingsApi`] used by the route tests.
#[cfg(test)]
pub struct FakeSettings {
    pub(super) tree: std::sync::Mutex<Value>,
    pub(super) state: std::sync::Mutex<Value>,
    /// What `get_time_status` answers; the shape micad's `status_json` serves.
    pub(super) time_status: std::sync::Mutex<Value>,
    /// What `get_storage_status` answers; the shape micad's storage
    /// `status_json` serves.
    pub(super) storage_status: std::sync::Mutex<Value>,
    /// What `get_system_info` answers; the shape micad's `info_json` serves.
    pub(super) system_info: std::sync::Mutex<Value>,
    /// What `get_telemetry` answers; the shape micad's `telemetry_json` serves.
    pub(super) telemetry: std::sync::Mutex<Value>,
    /// What `get_observed_network` answers; the shape micad's `observed_json`
    /// serves.
    pub(super) observed_network: std::sync::Mutex<Value>,
    /// What `get_failure_evidence` answers; the shape micad's failure
    /// evidence serves.
    pub(super) failure_evidence: std::sync::Mutex<Value>,
    /// When set, every diagnostic read sleeps this long before
    /// answering — how the snapshot collector's deadline is proven to be
    /// enforced rather than hoped for.
    pub(super) diagnostic_delay: std::sync::Mutex<Option<std::time::Duration>>,
    pub(super) get_log: std::sync::Mutex<Vec<String>>,
    pub(super) set_log: std::sync::Mutex<Vec<String>>,
    pub(super) power_log: std::sync::Mutex<Vec<String>>,
    /// Interfaces passed to `rotate_wireguard_key`, in call order, each
    /// paired with the public key handed back.
    ///
    /// The interface and the answer, never a private key: the fake has none to
    /// store because the trait has no method that would produce one.
    pub(super) rotations: std::sync::Mutex<Vec<(String, String)>>,
    /// How many times `set_transient_root_password` was called.
    ///
    /// A count, never the password. A fake that stored the password would let
    /// a test assert "the right password arrived" and pass while the real path
    /// leaks it somewhere else; there is nothing to assert about here except
    /// whether the call happened and how often.
    pub(super) transient_password_calls: std::sync::Mutex<usize>,
    pub(super) next_task: std::sync::atomic::AtomicU64,
    pub(super) tasks: std::sync::Mutex<std::collections::BTreeMap<String, TaskRecord>>,
    /// Update calls received, in order (`check`, `fetch`, `install <path>`,
    /// `mark <state> <slot>`, `reboot-override <s>`, `get_update_state`).
    pub(super) update_log: std::sync::Mutex<Vec<String>>,
    /// When set, every update action fails with a `zbus` `MethodError` of
    /// this fdo name and message — how a route test provokes the 409/422
    /// mappings the real micad produces.
    pub(super) update_refusal: std::sync::Mutex<Option<(&'static str, String)>>,
    /// When armed, the first reads of the `access` subtree rendezvous before
    /// they are answered. See [`AccessHold`].
    pub(super) access_hold: std::sync::Mutex<Option<std::sync::Arc<AccessHold>>>,
    /// How many `access` reads have been taken, so a hold engages for the
    /// first `parties` of them and for no others.
    pub(super) access_reads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FakeSettings {
    pub fn new(tree: Value) -> Self {
        Self {
            tree: std::sync::Mutex::new(tree),
            state: std::sync::Mutex::new(Value::Object(serde_json::Map::new())),
            time_status: std::sync::Mutex::new(serde_json::json!({
                "status": "synchronized",
                "synchronized": true,
            })),
            storage_status: std::sync::Mutex::new(serde_json::json!({
                "tiers": [],
                "namespaces": { "sharedCapacityTier": "data", "binds": [] },
                "media": [],
                "policy": {},
                "lifecycle": {},
            })),
            system_info: std::sync::Mutex::new(serde_json::json!({
                "machineId": { "available": true, "id": "0123456789abcdef0123456789abcdef" },
                "board": { "available": false, "detail": "fake" },
                "kernel": { "available": true, "release": "6.1.0-fake", "version": "#1" },
                "release": { "available": false, "detail": "fake" },
                "system": { "available": false, "detail": "fake" },
                "daemon": { "name": "micad", "version": "0.1.0", "commit": null },
                "packages": { "available": false, "detail": "fake" },
                "slot": { "available": false, "detail": "fake" },
                "uptime": { "available": true, "seconds": 7 },
            })),
            telemetry: std::sync::Mutex::new(serde_json::json!({
                "thermal": { "available": false, "detail": "fake" },
                "watchdog": { "available": false, "detail": "fake" },
                "reset": { "available": false, "reason": "unknown", "detail": "fake",
                           "evidence": { "watchdogBootstatus": [], "pstore": { "available": false, "detail": "fake" } } },
            })),
            observed_network: std::sync::Mutex::new(serde_json::json!({
                "interfaces": { "available": false, "detail": "fake" },
                "defaultRoutes": { "available": false, "detail": "fake" },
                "dns": { "available": false, "detail": "fake", "linkServers": [] },
                "wifi": { "available": false, "detail": "fake" },
                "capabilities": {
                    "wifi": { "supported": false, "interfaces": [], "detail": "fake" },
                    "bluetooth": { "supported": false, "adapters": [], "detail": "fake" },
                    "cellular": { "supported": false, "interfaces": [], "detail": "fake" },
                },
            })),
            failure_evidence: std::sync::Mutex::new(serde_json::json!({
                "journal": { "available": false, "detail": "fake" },
                "units": { "available": false, "detail": "fake" },
            })),
            diagnostic_delay: std::sync::Mutex::new(None),
            get_log: std::sync::Mutex::new(Vec::new()),
            set_log: std::sync::Mutex::new(Vec::new()),
            power_log: std::sync::Mutex::new(Vec::new()),
            transient_password_calls: std::sync::Mutex::new(0),
            rotations: std::sync::Mutex::new(Vec::new()),
            next_task: std::sync::atomic::AtomicU64::new(0),
            tasks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            update_log: std::sync::Mutex::new(Vec::new()),
            update_refusal: std::sync::Mutex::new(None),
            access_hold: std::sync::Mutex::new(None),
            access_reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Update actions received, in call order.
    pub fn update_calls(&self) -> Vec<String> {
        self.update_log.lock().unwrap().clone()
    }

    /// Make every subsequent update action fail as micad would: a bus
    /// `MethodError` under fdo `name` carrying `message`.
    pub fn refuse_updates(&self, name: &'static str, message: &str) {
        *self.update_refusal.lock().unwrap() = Some((name, message.to_string()));
    }

    /// Log one update action and fail it when a refusal is scripted.
    pub(super) fn update_call(&self, call: &str) -> anyhow::Result<()> {
        self.update_log.lock().unwrap().push(call.to_string());
        if let Some((name, message)) = self.update_refusal.lock().unwrap().clone() {
            let reply_to = zbus::message::Message::method_call("/com/mica/micad", "CheckUpdate")
                .expect("a well-formed method call")
                .build(&())
                .expect("an empty body serialises");
            let name = zbus::names::ErrorName::try_from(name).expect("a well-formed error name");
            return Err(zbus::Error::MethodError(name.into(), Some(message), reply_to).into());
        }
        Ok(())
    }

    pub(super) fn completed_task(&self, operation: &str, path: &str) -> String {
        let sequence = self
            .next_task
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let id = format!("fake-task-{sequence}");
        self.tasks.lock().unwrap().insert(
            id.clone(),
            TaskRecord {
                id: id.clone(),
                operation: operation.to_string(),
                dot_path: path.to_string(),
                source: "test".to_string(),
                status: "finished".to_string(),
                enqueued_at: "2026-08-31T00:00:00.000Z".to_string(),
                started_at: Some("2026-08-31T00:00:00.000Z".to_string()),
                finished_at: Some("2026-08-31T00:00:00.000Z".to_string()),
                outcome: Some("succeeded".to_string()),
                message: None,
                folded_count: 0,
            },
        );
        id
    }

    /// Rotations requested, as `(iface, public key answered)`, in call order.
    pub fn rotations(&self) -> Vec<(String, String)> {
        self.rotations.lock().unwrap().clone()
    }

    /// Replace what `get_time_status` answers.
    pub fn set_time_status(&self, value: Value) {
        *self.time_status.lock().unwrap() = value;
    }

    /// Replace what `get_storage_status` answers.
    pub fn set_storage_status(&self, value: Value) {
        *self.storage_status.lock().unwrap() = value;
    }

    /// Replace what `get_system_info` answers.
    pub fn set_system_info(&self, value: Value) {
        *self.system_info.lock().unwrap() = value;
    }

    /// Replace what `get_telemetry` answers.
    pub fn set_telemetry(&self, value: Value) {
        *self.telemetry.lock().unwrap() = value;
    }

    /// Replace what `get_observed_network` answers.
    pub fn set_observed_network(&self, value: Value) {
        *self.observed_network.lock().unwrap() = value;
    }

    /// Replace what `get_failure_evidence` answers.
    pub fn set_failure_evidence(&self, value: Value) {
        *self.failure_evidence.lock().unwrap() = value;
    }

    /// Make every diagnostic read sleep `delay` before answering.
    pub fn set_diagnostic_delay(&self, delay: std::time::Duration) {
        *self.diagnostic_delay.lock().unwrap() = Some(delay);
    }

    /// Hold the first `parties` reads of the `access` subtree at a rendezvous,
    /// each waiting at most `timeout` for the others.
    ///
    /// The seam a concurrent-claim test drives: it makes both claimants take
    /// the read half of the claim's check-then-act before either takes the
    /// write half. See [`AccessHold`] for what the bound means and for why
    /// this widens a window rather than inventing one.
    pub fn hold_access_reads(&self, parties: usize, timeout: std::time::Duration) {
        *self.access_hold.lock().unwrap() = Some(std::sync::Arc::new(AccessHold {
            barrier: tokio::sync::Barrier::new(parties),
            parties,
            timeout,
        }));
    }

    /// Hold this read at the armed rendezvous, if it is an `access` read and
    /// one of the first `parties` of them.
    pub(super) async fn hold_access_read(&self, path: &str) {
        if path != "access" {
            return;
        }
        // The lock is released before the await: a `std::sync` guard held
        // across one would deadlock the second claimant against the first.
        let hold = {
            let armed = self.access_hold.lock().unwrap();
            let Some(hold) = armed.as_ref() else { return };
            let taken = self
                .access_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if taken >= hold.parties {
                return;
            }
            hold.clone()
        };
        if tokio::time::timeout(hold.timeout, hold.barrier.wait())
            .await
            .is_err()
        {
            // The rendezvous did not fill, so the parties it was waiting for
            // are not coming. Disarm it rather than make every later read wait
            // the bound out on its own.
            *self.access_hold.lock().unwrap() = None;
        }
    }

    pub(super) async fn diagnostic_pause(&self) {
        let delay = *self.diagnostic_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }

    /// Insert `value` at top-level `key` of the live-state tree.
    pub fn set_state_entry(&self, key: &str, value: Value) {
        self.state
            .lock()
            .unwrap()
            .as_object_mut()
            .expect("state root is an object")
            .insert(key.to_string(), value);
    }

    /// Dot-paths passed to `set_settings`, in call order.
    pub fn set_paths(&self) -> Vec<String> {
        self.set_log.lock().unwrap().clone()
    }

    /// Power actions requested, in call order.
    pub fn power_calls(&self) -> Vec<String> {
        self.power_log.lock().unwrap().clone()
    }

    /// How many transient-password requests reached the backend.
    pub fn transient_password_calls(&self) -> usize {
        *self.transient_password_calls.lock().unwrap()
    }
}

/// Split `path` the way the real store splits it.
///
/// [`micad_settings::path_segments`] and not `str::split('.')`: a quoted
/// segment carries a dot as an ordinary character, so a fake that split
/// unconditionally would put a VLAN write at the two keys `"eth0` and `100"`
/// and let a route test assert the write "arrived".
#[cfg(test)]
pub(super) fn fake_segments(path: &str) -> anyhow::Result<Vec<String>> {
    micad_settings::path_segments(path).ok_or_else(|| anyhow::anyhow!("malformed path: `{path}`"))
}

#[cfg(test)]
pub(super) fn fake_get(root: &Value, path: &str) -> anyhow::Result<Value> {
    if path.is_empty() {
        return Ok(root.clone());
    }
    fake_segments(path)?
        .iter()
        .try_fold(root, |node, segment| node.get(segment))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("path not found: `{path}`"))
}

#[cfg(test)]
#[async_trait::async_trait]
impl SettingsApi for FakeSettings {
    async fn get_settings(&self, path: &str) -> anyhow::Result<Value> {
        self.get_log.lock().unwrap().push(path.to_string());
        let value = fake_get(&self.tree.lock().unwrap(), path);
        self.hold_access_read(path).await;
        value
    }

    async fn set_settings(&self, path: &str, value: &Value) -> anyhow::Result<String> {
        self.set_log.lock().unwrap().push(path.to_string());
        let mut segments = fake_segments(path)?;
        let last = segments.pop().expect("a path has at least one segment");
        let mut tree = self.tree.lock().unwrap();
        let mut node = &mut *tree;
        for segment in segments {
            node = node
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("not an object at `{segment}`"))?
                .entry(segment)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }
        node.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("not an object at `{last}`"))?
            .insert(last, value.clone());
        Ok(self.completed_task("settings-write", path))
    }

    async fn get_task(&self, id: &str) -> anyhow::Result<TaskRecord> {
        self.tasks
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| TaskNotFound(id.to_string()).into())
    }

    async fn get_state(&self, path: &str) -> anyhow::Result<Value> {
        if path == "tasks" {
            return serde_json::to_value(
                self.tasks
                    .lock()
                    .unwrap()
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .map_err(Into::into);
        }
        let mut state = fake_get(&self.state.lock().unwrap(), path)?;
        if path.is_empty() {
            let tasks = serde_json::to_value(
                self.tasks
                    .lock()
                    .unwrap()
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
            )?;
            if let Some(root) = state.as_object_mut() {
                root.insert("tasks".to_string(), tasks);
            }
        }
        Ok(state)
    }

    async fn get_time_status(&self) -> anyhow::Result<Value> {
        Ok(self.time_status.lock().unwrap().clone())
    }

    async fn get_storage_status(&self) -> anyhow::Result<Value> {
        self.diagnostic_pause().await;
        Ok(self.storage_status.lock().unwrap().clone())
    }

    async fn get_system_info(&self) -> anyhow::Result<Value> {
        self.diagnostic_pause().await;
        Ok(self.system_info.lock().unwrap().clone())
    }

    async fn get_telemetry(&self) -> anyhow::Result<Value> {
        self.diagnostic_pause().await;
        Ok(self.telemetry.lock().unwrap().clone())
    }

    async fn get_observed_network(&self) -> anyhow::Result<Value> {
        self.diagnostic_pause().await;
        Ok(self.observed_network.lock().unwrap().clone())
    }

    async fn get_failure_evidence(&self) -> anyhow::Result<Value> {
        self.diagnostic_pause().await;
        Ok(self.failure_evidence.lock().unwrap().clone())
    }

    /// Every source reads `lines` from micad's own echo; a source the fake
    /// does not know is refused as micad refuses it.
    async fn get_log(&self, source: &str) -> anyhow::Result<Value> {
        if !matches!(source, "micad" | "apid") {
            return Err(zbus::Error::MethodError(
                zbus::names::OwnedErrorName::try_from("org.freedesktop.DBus.Error.InvalidArgs")?,
                Some(format!("no log named {source:?}")),
                zbus::message::Message::method_call("/", "GetLog")?.build(&())?,
            )
            .into());
        }
        Ok(serde_json::json!({
            "source": source,
            "available": true,
            "lines": [
                format!("2026-09-28T10:00:00+00:00 {source}[1]: serving"),
                "2026-09-28T10:00:01+00:00 wpa_supplicant[2]: psk=hunter22 set".to_string(),
                "2026-09-28T10:00:02+00:00 kernel: link aa:bb:cc:dd:ee:ff up".to_string(),
            ],
            "truncated": false,
        }))
    }

    async fn reboot(&self) -> anyhow::Result<()> {
        self.power_log.lock().unwrap().push("reboot".to_string());
        Ok(())
    }

    async fn power_off(&self) -> anyhow::Result<()> {
        self.power_log.lock().unwrap().push("power_off".to_string());
        Ok(())
    }

    async fn set_transient_root_password(&self, _password: &str) -> anyhow::Result<String> {
        // The password is dropped here on purpose; see the field's comment.
        *self.transient_password_calls.lock().unwrap() += 1;
        Ok(self.completed_task("transient-password", "access.ssh"))
    }

    async fn rotate_wireguard_key(&self, iface: &str) -> anyhow::Result<String> {
        // A distinct answer per call, so a test can tell a fresh rotation from
        // a cached one. Base64 of 32 bytes, the shape a real public key has.
        let count = self.rotations.lock().unwrap().len();
        let public_key = micad_settings::encode_base64_nopad(&[count as u8; 32]);
        self.rotations
            .lock()
            .unwrap()
            .push((iface.to_string(), public_key.clone()));
        Ok(public_key)
    }

    async fn get_update_state(&self) -> anyhow::Result<Value> {
        // The fake's "refreshed" state is whatever the test seeded under the
        // live-state `update` key — the route's job is transport, not
        // derivation, which micad's own tests own.
        self.update_log
            .lock()
            .unwrap()
            .push("get_update_state".to_string());
        fake_get(&self.state.lock().unwrap(), "update")
    }

    async fn check_update(&self) -> anyhow::Result<()> {
        self.update_call("check")
    }

    async fn fetch_update(&self) -> anyhow::Result<()> {
        self.update_call("fetch")
    }

    async fn get_bluetooth(&self) -> anyhow::Result<Value> {
        // The shape micad answers, so a route test drives the document the
        // console actually reads.
        Ok(serde_json::json!({
            "enabled": true,
            "discoverable": false,
            "pin": "4211",
            "declared": { "AA:BB:CC:DD:EE:01": { "name": "phone", "trusted": true, "blocked": false } },
            "adapter": { "available": true, "address": "11:22:33:44:55:66", "alias": "edge-42", "powered": true, "discoverable": false, "discovering": false },
            "devices": { "available": true, "entries": [
                { "address": "AA:BB:CC:DD:EE:01", "name": "phone", "paired": true, "trusted": true, "blocked": false, "connected": false, "rssi": -55 },
            ] },
            "pending": null,
        }))
    }

    async fn set_bluetooth_discovery(&self, on: bool) -> anyhow::Result<()> {
        self.update_call(&format!("bluetooth discovery {on}"))
    }

    async fn pair_bluetooth_device(&self, address: &str) -> anyhow::Result<()> {
        self.update_call(&format!("bluetooth pair {address}"))
    }

    async fn confirm_bluetooth_pairing(&self, address: &str, accept: bool) -> anyhow::Result<()> {
        self.update_call(&format!("bluetooth confirm {address} {accept}"))
    }

    async fn remove_bluetooth_device(&self, address: &str) -> anyhow::Result<()> {
        self.update_call(&format!("bluetooth remove {address}"))
    }

    async fn scan_wifi(&self) -> anyhow::Result<Value> {
        self.update_log
            .lock()
            .unwrap()
            .push("scan_wifi".to_string());
        Ok(serde_json::json!({
            "available": true,
            "interface": "wlan0",
            "networks": [
                { "ssid": "workshop", "bssid": "aa:bb:cc:dd:ee:01", "signalDbm": -42, "flags": "[WPA2-PSK-CCMP][ESS]" },
            ],
        }))
    }

    async fn import_update(&self, path: &str) -> anyhow::Result<()> {
        self.update_call(&format!("import {path}"))
    }

    async fn install_update(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.update_call(&format!("install {deployment_id}"))
    }

    async fn confirm_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.update_call(&format!("confirm {deployment_id}"))
    }

    async fn reject_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.update_call(&format!("reject {deployment_id}"))
    }

    async fn rollback_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.update_call(&format!("rollback {deployment_id}"))
    }

    async fn set_reboot_override(&self, seconds: u32) -> anyhow::Result<Value> {
        self.update_call(&format!("reboot-override {seconds}"))?;
        Ok(serde_json::json!({
            "until": "2026-09-02T00:10:00Z",
            "requestedBy": ":1.9",
        }))
    }

    async fn set_update_config(&self, patch: &Value) -> anyhow::Result<Value> {
        self.update_call(&format!("config {patch}"))?;
        // The patch echoed as the document, which is what merging it over an
        // empty one produces. The merge itself is micad's and is tested there:
        // this route's job is the transport, the authority and the audit, and
        // a fake that re-implemented the precedence would be a second answer
        // to a question the library already answers once.
        Ok(patch.clone())
    }
}
