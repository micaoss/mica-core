//! The `com.mica.micad` D-Bus interface.

use crate::deployment;
use crate::storage_status;
use crate::system_info;
use crate::telemetry;
use crate::time_status::status_json;
use crate::transient;
use micad_settings::json_path_get;
use serde_json::Value;
use zbus::fdo;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;

use super::*;

/// Whole seconds since boot, from the first field of `/proc/uptime`.
///
/// `None` on an unreadable or malformed file — a soft failure rather than a
/// panic, because `GetState` must keep answering for every other fact when
/// this one is missing.
pub(super) fn read_uptime_seconds() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs: f64 = contents.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

#[zbus::interface(name = "com.mica.micad1")]
impl MicadService {
    /// JSON-encoded settings value at dot-path `path` (`""` = whole tree).
    pub(super) async fn get_settings(&self, path: &str) -> Result<String, SettingsFault> {
        let inner = self.inner.read().await;
        let value = inner.settings.get(path).map_err(to_bus_error)?;
        Ok(value.to_string())
    }

    /// Parse `value_json`, persist it atomically, enqueue the overlapping
    /// reconcile, and return its task id.
    pub(super) async fn set_settings(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        path: &str,
        value_json: &str,
    ) -> Result<String, SettingsFault> {
        let value: Value = serde_json::from_str(value_json).map_err(|err| {
            SettingsFault::Fdo(fdo::Error::InvalidArgs(format!(
                "invalid JSON value: {err}"
            )))
        })?;
        self.persist_setting(path, value)
            .await
            .map_err(to_bus_error)?;
        Self::settings_changed(&emitter, path, value_json)
            .await
            .map_err(|err| {
                SettingsFault::Fdo(fdo::Error::Failed(format!("emit SettingsChanged: {err}")))
            })?;
        let task = self
            .enqueue_apply(&emitter, "settings-write", path, sender_of(&header))
            .await;
        Ok(task.id)
    }

    /// JSON-encoded task record for `id`.
    pub(super) async fn get_task(&self, id: &str) -> Result<String, SettingsFault> {
        let task = self
            .apply_queue
            .get(id)
            .await
            .ok_or_else(|| SettingsFault::NotFound(format!("task not found: `{id}`")))?;
        serde_json::to_string(&task).map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!("serialize task {id}: {err}")))
        })
    }

    /// JSON-encoded live-state subtree at dot-path `path` (`""` = whole tree).
    pub(super) async fn get_state(&self, path: &str) -> Result<String, SettingsFault> {
        if path == "uptime" {
            let secs = read_uptime_seconds().ok_or_else(|| {
                SettingsFault::Fdo(fdo::Error::Failed("read /proc/uptime".to_string()))
            })?;
            return Ok(Value::from(secs).to_string());
        }
        let inner = self.inner.read().await;
        if path.is_empty() {
            let mut root = inner.state.clone();
            if let (Some(secs), Some(map)) = (read_uptime_seconds(), root.as_object_mut()) {
                map.insert("uptime".to_string(), Value::from(secs));
            }
            return Ok(root.to_string());
        }
        // The dot-path names nothing in the tree. That is [`NOT_FOUND_ERROR`],
        // the same name `rotate_wireguard_key` raises for an interface the
        // settings do not declare, and not `InvalidArgs`: the argument is
        // well-formed, there is simply no value under it. Under one shared
        // name apid could only tell the two apart by knowing that on the state
        // route the name had a single producer, and it read the name against
        // that private fact to answer 404. Naming the condition here is what lets that reading go.
        let value = json_path_get(&inner.state, path)
            .ok_or_else(|| SettingsFault::NotFound(format!("state path not found: `{path}`")))?;
        Ok(value.to_string())
    }

    /// JSON time-synchronization status, observed from timesyncd at call
    /// time and classified by [`crate::time_status::classify`].
    pub(super) async fn get_time_status(&self) -> Result<String, SettingsFault> {
        let evidence = self.time_status.observe().await.map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!(
                "observe time synchronization: {err:#}"
            )))
        })?;
        Ok(status_json(&evidence).to_string())
    }

    /// JSON storage status: the fixed tiers, the physical media and the
    /// low-space policy, observed at call time.
    pub(super) async fn get_storage_status(&self) -> Result<String, SettingsFault> {
        let evidence = self.storage_status.observe().await.map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!("observe storage: {err:#}")))
        })?;
        Ok(storage_status::status_json(&evidence, &self.storage_pressure).to_string())
    }

    /// JSON snapshot returned by systemd-networkd's live `Describe` method.
    pub(super) async fn get_network_state(&self) -> Result<String, SettingsFault> {
        self.network_state
            .describe()
            .await
            .map(|value| value.to_string())
            .map_err(|err| {
                SettingsFault::Fdo(fdo::Error::Failed(format!(
                    "observe network state: {err:#}"
                )))
            })
    }

    /// JSON observed network state: link/carrier, addresses, DHCP
    /// lease, default routes, DNS reachability, Wi-Fi association and the
    /// radio/modem capabilities, observed at call time and DISTINCT from the
    /// desired `network` settings, which this method never reads.
    pub(super) async fn get_observed_network(&self) -> Result<String, SettingsFault> {
        self.network_state
            .observe()
            .await
            .map(|value| value.to_string())
            .map_err(|err| {
                SettingsFault::Fdo(fdo::Error::Failed(format!("observe network: {err:#}")))
            })
    }

    /// JSON system information: machine id, board, kernel,
    /// release, the image version with its git stamp and build date, the
    /// installed packages, the running deployment and the uptime — every one read
    /// at call time from the seam that already carries it, none restated.
    pub(super) async fn get_system_info(&self) -> Result<String, SettingsFault> {
        let evidence = self.system_info.observe().await.map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!(
                "observe system information: {err:#}"
            )))
        })?;
        let deployment = self.deployment_evidence().await;
        Ok(system_info::info_json(
            &evidence,
            deployment.as_ref(),
            &system_info::DaemonIdentity::this_build(),
        )
        .to_string())
    }

    /// JSON board telemetry: temperature, watchdog and the reset
    /// reason the kernel's generic sources support, observed at call time.
    /// Absence is explicit; nothing here reads a vendor register.
    /// The containers this device declares, joined with what mica-containerd
    /// says of them (phase, health, restarts, exit code) -- each side named,
    /// never merged.
    ///
    /// A declared container the engine has never heard of and a running
    /// container nobody declared are both real states, and a merged answer
    /// could not tell them apart.
    pub(super) async fn get_containers(&self) -> Result<String, SettingsFault> {
        self.require_feature(micad_settings::Feature::Containers)
            .map_err(SettingsFault::Fdo)?;
        let settings = self.inner.read().await.settings.clone();
        let declared = serde_json::to_value(&settings.container.units)
            .unwrap_or_else(|_| serde_json::json!({}));
        Ok(serde_json::json!({
            "enabled": settings.container.enabled,
            "declared": declared,
            "engine": self.daemon_containers().await,
            "images": self.engine.images().await,
        })
        .to_string())
    }

    /// Scan for WiFi networks on the station's radio and answer what it found.
    ///
    /// The interface is the one `wifi.client` declares -- a scan is a thing
    /// the station role does, not an arbitrary radio operation, so there is no
    /// interface argument to point somewhere else.
    pub(super) async fn scan_wifi(&self) -> Result<String, SettingsFault> {
        self.scan_wifi_now().await
    }

    /// The declared Bluetooth trust list, what BlueZ reports, the pairing code
    /// and whatever is waiting to be confirmed.
    pub(super) async fn get_bluetooth(&self) -> Result<String, SettingsFault> {
        self.bluetooth_view().await
    }

    /// Start or stop a scan.
    ///
    /// A scan is bounded by BlueZ's own discovery and by the console stopping
    /// it; what this refuses is starting one on a device whose adapter is
    /// switched off, where it would fail with a bus error about an object that
    /// does not exist.
    pub(super) async fn set_bluetooth_discovery(&self, on: bool) -> fdo::Result<()> {
        self.require_bluetooth().await?;
        self.bluetooth
            .set_discovery(on)
            .await
            .map_err(|err| fdo::Error::Failed(format!("bluetooth discovery: {err:#}")))
    }

    /// Pair with a device, then record it in the trust list.
    ///
    /// The record is the point: BlueZ remembers the keys, and the settings
    /// tree remembers that this device is one this appliance trusts. A reset
    /// clears the second, and the reconciler then clears the first.
    pub(super) async fn pair_bluetooth_device(&self, address: &str) -> fdo::Result<()> {
        self.pair_bluetooth_device_now(address).await
    }

    /// Answer the passkey the agent is holding.
    pub(super) async fn confirm_bluetooth_pairing(
        &self,
        address: &str,
        accept: bool,
    ) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        if self.agent.answer(address, accept).await {
            Ok(())
        } else {
            Err(fdo::Error::Failed(format!(
                "no pairing confirmation is waiting for {address}"
            )))
        }
    }

    /// Drop a device: out of the trust list, and out of the adapter.
    pub(super) async fn remove_bluetooth_device(&self, address: &str) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        let _apply = self.apply_lock.lock().await;
        let mut devices = self.inner.read().await.settings.bluetooth.devices.clone();
        if devices.remove(address).is_none() {
            return Err(fdo::Error::InvalidArgs(format!(
                "no device named {address:?} is declared on this device"
            )));
        }
        let value = serde_json::to_value(&devices).unwrap_or(Value::Null);
        self.persist_setting("bluetooth.devices", value)
            .await
            .map_err(|err| fdo::Error::Failed(format!("persist the trust list: {err}")))?;
        drop(_apply);
        // The adapter is told directly as well as through the reconcile: the
        // keys should go now, not at the next pass.
        let _ = self.bluetooth.remove(address).await;
        self.apply_subtree("bluetooth").await;
        Ok(())
    }

    /// Start a declared container, through mica-containerd.
    pub(super) async fn start_container(&self, name: &str) -> fdo::Result<()> {
        self.container_action(name, ContainerAction::Start).await
    }

    /// Stop it.
    pub(super) async fn stop_container(&self, name: &str) -> fdo::Result<()> {
        self.container_action(name, ContainerAction::Stop).await
    }

    /// Restart it, which is what a changed image tag needs.
    pub(super) async fn restart_container(&self, name: &str) -> fdo::Result<()> {
        self.container_action(name, ContainerAction::Restart).await
    }

    pub(super) async fn get_telemetry(&self) -> Result<String, SettingsFault> {
        let evidence = self.telemetry.observe().await.map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!("observe telemetry: {err:#}")))
        })?;
        Ok(telemetry::telemetry_json(&evidence).to_string())
    }

    /// JSON: the newest lines of one allowlisted service's log, bounded, or
    /// the reason they could not be read ([`crate::logs`]). A source that is
    /// not on the list, or belongs to a feature the product leaves out, is
    /// refused; the scrubbing is apid's, before a line leaves the device.
    pub(super) async fn get_log(&self, source: &str) -> fdo::Result<String> {
        let features = self.features.clone();
        let source = crate::logs::source(source, &features)
            .map_err(|err| fdo::Error::InvalidArgs(format!("{err:#}")))?;
        Ok(crate::logs::log_json(self.logs.as_ref(), source)
            .await
            .to_string())
    }

    /// JSON failure evidence for the diagnostic snapshot: the
    /// units systemd holds failed and a bounded excerpt of this boot's
    /// journal at warning and worse. Bounded in size and time by
    /// [`crate::diagnostics`]; the redaction is apid's, at the snapshot.
    pub(super) async fn get_failure_evidence(&self) -> Result<String, SettingsFault> {
        self.failure_evidence
            .observe()
            .await
            .map(|value| value.to_string())
            .map_err(|err| {
                SettingsFault::Fdo(fdo::Error::Failed(format!(
                    "observe failure evidence: {err:#}"
                )))
            })
    }

    /// Record a component health report in the live-state tree under
    /// `health.<component>` as `{"status": ..., "detail": ...}`.
    ///
    /// Used by the boot health gate (`mica-health`) to surface non-fatal
    /// pressure — a full `/var`, for example — without failing the gate.
    pub(super) async fn report_health(
        &self,
        component: &str,
        status: &str,
        detail: &str,
    ) -> fdo::Result<()> {
        self.record_health_report(component, status, detail).await
    }

    /// Drop a DISCONNECTED service from the registry (`crate::scan`).
    ///
    /// Exported as `ForgetService`. Refuses a service that is still connected,
    /// and a name the registry does not carry, with
    /// [`InvalidArgs`](fdo::Error::InvalidArgs).
    pub(super) async fn forget_service(&self, bus_name: &str) -> fdo::Result<()> {
        let registry = self.registry.as_ref().ok_or_else(|| {
            fdo::Error::Failed("no service registry: this daemon runs no scan".to_string())
        })?;
        let snapshot = registry.forget(bus_name)?;
        self.publish_services(snapshot).await;
        tracing::info!(service = bus_name, "service forgotten by request");
        Ok(())
    }

    /// Reboot the appliance through systemd.
    ///
    /// The request is logged and recorded in the live-state tree before the
    /// call is made.
    pub(super) async fn reboot(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.request_reboot(sender_of(&header)).await
    }

    /// Power the appliance off through systemd.
    ///
    /// The request is logged and recorded in the live-state tree before the
    /// call is made.
    pub(super) async fn power_off(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.request_power_off(sender_of(&header)).await
    }

    /// Install a signed deployment already verified in the acquisition workspace.
    pub(super) async fn install_update(
        &self,
        #[zbus(header)] header: Header<'_>,
        deployment_id: &str,
    ) -> fdo::Result<()> {
        if !deployment::valid_id(deployment_id) {
            return Err(fdo::Error::InvalidArgs("invalid deployment ID".into()));
        }
        // The id of whatever is staged: a deployment's descriptor, or a core
        // set under its own name.
        let verified = self.update.verified_dir();
        let core = verified.join(format!(
            "{}{deployment_id}.json",
            deployment::CORE_DESCRIPTOR_PREFIX
        ));
        let path = if core.symlink_metadata().is_ok() {
            core
        } else {
            verified.join(format!("{deployment_id}.json"))
        };
        self.request_install(sender_of(&header), &path.to_string_lossy())
            .await
    }

    /// Refresh native boot, deployment and rollback evidence, retaining the
    /// installation, action and acquisition lifecycle records.
    pub(super) async fn get_update_state(&self) -> fdo::Result<String> {
        self.refresh_update_state().await
    }

    pub(super) async fn confirm_deployment(
        &self,
        #[zbus(header)] header: Header<'_>,
        deployment_id: &str,
    ) -> fdo::Result<()> {
        self.request_deployment_action(sender_of(&header), "confirm", deployment_id)
            .await
    }

    pub(super) async fn reject_deployment(
        &self,
        #[zbus(header)] header: Header<'_>,
        deployment_id: &str,
    ) -> fdo::Result<()> {
        self.request_deployment_action(sender_of(&header), "reject", deployment_id)
            .await
    }

    pub(super) async fn rollback_deployment(
        &self,
        #[zbus(header)] header: Header<'_>,
        deployment_id: &str,
    ) -> fdo::Result<()> {
        self.request_deployment_action(sender_of(&header), "rollback", deployment_id)
            .await
    }

    /// Run an update metadata check (`mica-deploy sync` + `check`) on a
    /// background task.
    pub(super) async fn check_update(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.update
            .request_check(sender_of(&header))
            .await
            .map_err(refusal_to_fdo)
    }

    /// Download the selected descriptor (`mica-deploy fetch`) on a background
    /// task; on success the verified descriptor path is recorded and the
    /// lifecycle state becomes `ready`.
    ///
    /// Exported as `FetchUpdate`. The same admission and refusal shape as
    /// `CheckUpdate`, plus the metered-mode download refusal.
    pub(super) async fn fetch_update(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        self.update
            .request_fetch(sender_of(&header))
            .await
            .map_err(refusal_to_fdo)
    }

    /// Import an offline `MICAUPD1` archive somebody uploaded.
    ///
    /// The path is bounded to the acquisition workspace's own upload
    /// directory. The method takes a path because the archive is measured in
    /// hundreds of megabytes and the bus is not how those travel; apid streams
    /// it to disk and names it here. Bounding the path is what keeps the
    /// method from being a way to hand `mica-deploy` an arbitrary file.
    pub(super) async fn import_update(
        &self,
        #[zbus(header)] header: Header<'_>,
        path: &str,
    ) -> fdo::Result<()> {
        let archive = std::path::Path::new(path);
        let uploads = self.update.uploads_dir();
        if archive.parent() != Some(uploads.as_path()) || archive.file_name().is_none() {
            return Err(fdo::Error::InvalidArgs(format!(
                "an imported archive is a file directly under {}",
                uploads.display()
            )));
        }
        self.update
            .request_import(sender_of(&header), archive)
            .await
            .map_err(refusal_to_fdo)
    }

    /// Arm the bounded administrative override of the safe-to-reboot gate
    /// for `seconds`; answers the recorded override as JSON.
    ///
    /// Exported as `SetRebootOverride`. The override lifts health-report
    /// blocks only — never an install in flight — and expires on its own;
    /// there is deliberately no member that disarms the gate permanently.
    pub(super) async fn set_reboot_override(
        &self,
        #[zbus(header)] header: Header<'_>,
        seconds: u32,
    ) -> fdo::Result<String> {
        let record = self
            .update
            .set_reboot_override(sender_of(&header), u64::from(seconds))
            .await
            .map_err(refusal_to_fdo)?;
        Ok(record.to_string())
    }

    /// Write the operator's update configuration document; answers the
    /// document as saved, as JSON.
    pub(super) async fn set_update_config(
        &self,
        #[zbus(header)] header: Header<'_>,
        patch_json: &str,
    ) -> fdo::Result<String> {
        let document = self
            .update
            .write_config(sender_of(&header), patch_json)
            .await
            .map_err(refusal_to_fdo)?;
        Ok(document.to_string())
    }

    /// Set a TRANSIENT root password, then re-apply the SSH subtree.
    ///
    /// Exported as `SetTransientRootPassword`. The password lives until the
    /// next boot, when `mica-shadow-reconcile` clears the root hash it wrote;
    /// persistent access is by SSH public key.
    #[zbus(name = "SetTransientRootPassword")]
    pub(super) async fn enqueue_transient_root_password(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        password: &str,
    ) -> fdo::Result<String> {
        // Two concerns share this shape. Serialization: zbus dispatches `&self`
        // methods concurrently, and two unserialized writers would interleave
        // read-modify-write cycles on one shadow file through one fixed temp
        // name — so the write happens under the same lock every other mutating
        // method takes. Blocking: the bcrypt hash inside costs hundreds of
        // milliseconds of CPU, which must not stall the bus dispatcher, so the
        // whole write runs off the async scheduler while the guard is held.
        let _apply = self.apply_lock.lock().await;
        let shadow_path = self.shadow_path.clone();
        let password = password.to_string();
        tokio::task::spawn_blocking(move || {
            transient::set_transient_root_password(&shadow_path, &password)
        })
        .await
        .map_err(|err| fdo::Error::Failed(format!("transient password task: {err}")))?
        .map_err(transient_to_fdo)?;
        let task = self
            .enqueue_apply(
                &emitter,
                "transient-password",
                "access.ssh",
                sender_of(&header),
            )
            .await;
        Ok(task.id)
    }

    /// Draw a new private key for the WireGuard interface `iface` and return
    /// its new public key.
    pub(super) async fn rotate_wireguard_key(&self, iface: &str) -> Result<String, SettingsFault> {
        self.rotate_wireguard_key_now(iface).await
    }

    /// Emitted after a successful `SetSettings` with the changed dot-path and
    /// its new JSON-encoded value.
    #[zbus(signal)]
    pub(super) async fn settings_changed(
        emitter: &SignalEmitter<'_>,
        path: &str,
        value_json: &str,
    ) -> zbus::Result<()>;

    /// Emitted whenever an apply task is queued, starts or finishes. The body
    /// is one JSON-encoded [`TaskRecord`].
    #[zbus(signal)]
    pub(super) async fn task_changed(
        emitter: &SignalEmitter<'_>,
        task_json: &str,
    ) -> zbus::Result<()>;
}
