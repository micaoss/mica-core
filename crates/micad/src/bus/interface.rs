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

/// The newest hostapd lines the access point's down reason is read from.
const AP_LOG_LINES: usize = 5;

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

/// The longest dot-path every one of `paths` lies under; `""`, the whole
/// tree, when they share no first segment.
pub(super) fn shared_path(paths: &[String]) -> String {
    let mut shared: Option<Vec<String>> = None;
    for path in paths {
        let segments = micad_settings::path_segments(path).unwrap_or_default();
        shared = Some(match shared {
            None => segments,
            Some(prefix) => prefix
                .into_iter()
                .zip(segments)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    shared
        .unwrap_or_default()
        .iter()
        .map(|segment| micad_settings::quote_path_segment(segment))
        .collect::<Vec<_>>()
        .join(".")
}

/// Each of `values_json` parsed, paired with its path.
fn parse_writes(
    paths: &[String],
    values_json: &[String],
) -> Result<Vec<(String, Value)>, SettingsFault> {
    paths
        .iter()
        .zip(values_json)
        .map(|(path, value_json)| {
            serde_json::from_str(value_json)
                .map(|value| (path.clone(), value))
                .map_err(|err| {
                    SettingsFault::Fdo(fdo::Error::InvalidArgs(format!(
                        "invalid JSON value for `{path}`: {err}"
                    )))
                })
        })
        .collect()
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
        self.refuse_while_wifi_locked(std::slice::from_ref(&path.to_string()))?;
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

    /// Parse each of `values_json`, persist it at the matching dot-path of
    /// `paths` as one atomic write, enqueue one reconcile of what they share,
    /// and return its task id.
    pub(super) async fn set_settings_many(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        paths: Vec<String>,
        values_json: Vec<String>,
    ) -> Result<String, SettingsFault> {
        if paths.is_empty() || paths.len() != values_json.len() {
            return Err(SettingsFault::Fdo(fdo::Error::InvalidArgs(
                "SetSettingsMany takes one value per path, and at least one".to_string(),
            )));
        }
        let writes = parse_writes(&paths, &values_json)?;
        self.refuse_while_wifi_locked(&paths)?;
        self.persist_settings(&writes).await.map_err(to_bus_error)?;
        for (path, value_json) in paths.iter().zip(&values_json) {
            Self::settings_changed(&emitter, path, value_json)
                .await
                .map_err(|err| {
                    SettingsFault::Fdo(fdo::Error::Failed(format!("emit SettingsChanged: {err}")))
                })?;
        }
        let task = self
            .enqueue_apply(
                &emitter,
                "settings-write",
                &shared_path(&paths),
                sender_of(&header),
            )
            .await;
        Ok(task.id)
    }

    /// Claim the device: [`Self::set_settings_many`] of writes that give it
    /// an administrator, refused with `AlreadyClaimed` when it has one, so
    /// the console's setup and BLE provisioning cannot both claim it. Then
    /// the BLE verifier of `password`, the administrator password the writes
    /// carry the hash of.
    pub(super) async fn claim(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        paths: Vec<String>,
        values_json: Vec<String>,
        password: String,
    ) -> Result<String, SettingsFault> {
        if paths.is_empty() || paths.len() != values_json.len() {
            return Err(SettingsFault::Fdo(fdo::Error::InvalidArgs(
                "Claim takes one value per path, and at least one".to_string(),
            )));
        }
        let writes = parse_writes(&paths, &values_json)?;
        self.refuse_while_wifi_locked(&paths)?;
        self.persist_claim(&writes).await.map_err(to_bus_error)?;
        for (path, value_json) in paths.iter().zip(&values_json) {
            Self::settings_changed(&emitter, path, value_json)
                .await
                .map_err(|err| {
                    SettingsFault::Fdo(fdo::Error::Failed(format!("emit SettingsChanged: {err}")))
                })?;
        }
        let task = self
            .enqueue_apply(
                &emitter,
                "settings-write",
                &shared_path(&paths),
                sender_of(&header),
            )
            .await;
        // A phone provisioning the unclaimed device is told and let go.
        self.provisioning.claimed_elsewhere();
        // The claim landed; a verifier that could not be made is made at the
        // next login.
        if let Err(err) = self.provisioning.password_set(password) {
            tracing::warn!(error = %format!("{err:#}"), "the BLE verifier could not be made at the claim");
        }
        Ok(task.id)
    }

    /// The administrator password was changed to `password`: replace the BLE
    /// verifier, so the old password stops working over Bluetooth. The old
    /// one is gone when this answers; the new one follows in about two
    /// seconds.
    pub(super) async fn admin_password_set(&self, password: String) -> fdo::Result<()> {
        self.provisioning
            .password_set(password)
            .map_err(|err| fdo::Error::Failed(format!("replace the BLE verifier: {err:#}")))
    }

    /// The administrator logged in to the console with `password`, which the
    /// caller verified: end a BLE lockout and make the verifier if there is
    /// none.
    pub(super) async fn admin_logged_in(&self, password: String) -> fdo::Result<()> {
        self.provisioning
            .logged_in(password)
            .map_err(|err| fdo::Error::Failed(format!("BLE provisioning at login: {err:#}")))
    }

    /// BLE provisioning's state (`docs/ble-provisioning.md`): `state`,
    /// `trigger`, `expiresAt`, `code` (a manual window's), `autoWhenOffline`,
    /// `verifierReady`, `lockedUntil`, `wifiLocked` and `history`.
    pub(super) async fn get_ble_provisioning(&self) -> fdo::Result<String> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        self.provisioning
            .status()
            .await
            .map(|status| status.to_string())
            .ok_or_else(|| fdo::Error::Failed("BLE provisioning is not running".to_string()))
    }

    /// Open a `manual` window for `seconds` (300, 600 or 1800), answering
    /// its temp code. Refused `AccessDenied` (`not_claimed: …`, `locked: …`)
    /// on an unclaimed or locked-out device.
    pub(super) async fn open_ble_provisioning(&self, seconds: u64) -> fdo::Result<String> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        self.provisioning
            .open_manual(seconds)
            .await
            .map_err(|reason| match reason {
                "invalid_duration" => fdo::Error::InvalidArgs(
                    "a provisioning window lasts 300, 600 or 1800 seconds".to_string(),
                ),
                "not_claimed" => fdo::Error::AccessDenied(
                    "not_claimed: an unclaimed device opens its own window at every boot"
                        .to_string(),
                ),
                "locked" => fdo::Error::AccessDenied(
                    "locked: too many failed attempts; a console login ends the lockout"
                        .to_string(),
                ),
                _ => fdo::Error::Failed("BLE provisioning is not running".to_string()),
            })
    }

    /// Close the `manual` window.
    pub(super) async fn close_ble_provisioning(&self) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        self.provisioning.close_manual().await;
        Ok(())
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
        let mut observed = self.network_state.observe().await.map_err(|err| {
            SettingsFault::Fdo(fdo::Error::Failed(format!("observe network: {err:#}")))
        })?;
        let (wifi, ap_state) = {
            let inner = self.inner.read().await;
            (
                inner.settings.wifi.clone(),
                inner.state.get("wifiAp").cloned(),
            )
        };
        // hostapd's own words are read only when they can explain something.
        let mut lines = Vec::new();
        if crate::network_state::failed(ap_state.as_ref())
            && let Ok(source) = crate::logs::source("wifi-ap", &self.features)
        {
            let log = crate::logs::log_json(self.logs.as_ref(), source).await;
            lines = log
                .get("lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            lines.drain(..lines.len().saturating_sub(AP_LOG_LINES));
        }
        crate::network_state::annotate_access_point(
            &mut observed,
            &wifi,
            ap_state.as_ref(),
            &lines,
        );
        Ok(observed.to_string())
    }

    /// JSON: each of `servers`, IPv4 addresses, asked directly for one `A`
    /// record of the probe name over UDP, all at once and each within three
    /// seconds: `[{server, reachable, latencyMs}]` in their order.
    pub(super) async fn test_dns_servers(&self, servers: Vec<String>) -> fdo::Result<String> {
        if servers.is_empty() || servers.len() > micad_settings::MAX_GLOBAL_DNS_SERVERS {
            return Err(fdo::Error::InvalidArgs(format!(
                "test 1 to {} servers",
                micad_settings::MAX_GLOBAL_DNS_SERVERS
            )));
        }
        let addresses = servers
            .iter()
            .map(|server| {
                server.parse::<std::net::Ipv4Addr>().map_err(|_| {
                    fdo::Error::InvalidArgs(format!("{server:?} is not an IPv4 address"))
                })
            })
            .collect::<fdo::Result<Vec<_>>>()?;
        let results = crate::network_state::test_dns_servers(
            &addresses,
            53,
            crate::network_state::DNS_PROBE_TIMEOUT,
        )
        .await;
        Ok(serde_json::to_string(&results).unwrap_or_default())
    }

    /// JSON: the last resolution check, `{ok, name, latencyMs, checkedAt,
    /// detail?}`, or `null` before the first.
    pub(super) async fn get_dns_probe(&self) -> fdo::Result<String> {
        let last = match &self.dns_probe {
            Some(probe) => probe.last().await,
            None => None,
        };
        Ok(serde_json::to_string(&last).unwrap_or_default())
    }

    /// Run the resolution check now; answers it as `GetDnsProbe` would.
    pub(super) async fn probe_dns(&self) -> fdo::Result<String> {
        let Some(probe) = &self.dns_probe else {
            return Err(fdo::Error::NotSupported(
                "this daemon runs no resolution check".to_string(),
            ));
        };
        Ok(serde_json::to_string(&probe.probe().await).unwrap_or_default())
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
    /// The projects this device declares, beside what mica-containerd says
    /// of their containers (phase, health, restarts, exit code) -- each side
    /// named, never merged.
    ///
    /// A declared project the engine has not taken and a project it still
    /// holds that nobody declares are both real states, and a merged answer
    /// could not tell them apart.
    pub(super) async fn get_containers(&self) -> Result<String, SettingsFault> {
        self.require_feature(micad_settings::Feature::Containers)
            .map_err(SettingsFault::Fdo)?;
        let settings = self.inner.read().await.settings.clone();
        Ok(serde_json::json!({
            "enabled": settings.container.enabled,
            "projects": settings.container.projects,
            "projectDirs": settings.container.project_dirs,
            "containerEngine": self.container_engine.as_str(),
            "engine": self.daemon_containers().await,
            "images": self.engine.images().await,
        })
        .to_string())
    }

    /// JSON: `text` checked as project `project`'s Compose file, as a write
    /// of it would be, in the project's stored directory: `{"ok": true,
    /// "containers"}`, or `{"ok": false, "detail", "problems"}`. Needs no
    /// daemon.
    pub(super) async fn check_container_project(
        &self,
        project: &str,
        text: &str,
    ) -> fdo::Result<String> {
        Ok(self
            .check_container_project_now(project, text, None)
            .await?
            .to_string())
    }

    /// JSON: `text` checked as project `project`'s Compose file with its
    /// relative host paths in `project_dir`, an absolute path, as a write
    /// declaring that directory would check it.
    pub(super) async fn check_container_project_in(
        &self,
        project: &str,
        text: &str,
        project_dir: &str,
    ) -> fdo::Result<String> {
        Ok(self
            .check_container_project_now(project, text, Some(project_dir))
            .await?
            .to_string())
    }

    /// The last `tail` lines (1 to 1000) of one declared service's log,
    /// `<project>/<service>`.
    pub(super) async fn get_container_log(&self, name: &str, tail: u32) -> fdo::Result<String> {
        self.container_log(name, tail).await
    }

    /// JSON: the user's services, every one declared or in `mica-unit`'s
    /// directory, with whether micad manages it, whether it is enabled, what
    /// the init says of it and the app it is part of: `{init, available,
    /// units}`.
    pub(super) async fn get_units(&self) -> fdo::Result<String> {
        let services = self.inner.read().await.settings.services.clone();
        let mut live = self
            .user_units
            .live(&services)
            .await
            .map_err(|err| fdo::Error::Failed(format!("{err:#}")))?;
        let owners = self.apps.unit_owners();
        if let Some(units) = live["units"].as_array_mut() {
            for unit in units {
                if let Some(app) = unit["name"].as_str().and_then(|name| owners.get(name)) {
                    unit["app"] = Value::from(app.clone());
                }
            }
        }
        Ok(live.to_string())
    }

    /// Start an installed user service, managed or put there by hand.
    pub(super) async fn start_unit(&self, name: &str) -> fdo::Result<()> {
        self.unit_action(name, crate::user_units::Verb::Start).await
    }

    /// Stop it.
    pub(super) async fn stop_unit(&self, name: &str) -> fdo::Result<()> {
        self.unit_action(name, crate::user_units::Verb::Stop).await
    }

    /// Restart it.
    pub(super) async fn restart_unit(&self, name: &str) -> fdo::Result<()> {
        self.unit_action(name, crate::user_units::Verb::Restart)
            .await
    }

    /// JSON: an installed user service's newest log lines, as `GetLog`
    /// answers one of the device's.
    pub(super) async fn get_unit_log(&self, name: &str) -> fdo::Result<String> {
        self.user_units
            .require_installed(name)
            .await
            .map_err(|err| fdo::Error::InvalidArgs(format!("{err:#}")))?;
        let source = self.user_units.log_source(name);
        Ok(crate::logs::log_json(self.logs.as_ref(), &source)
            .await
            .to_string())
    }

    /// JSON: every installed app, `{available, arch, apps}`.
    pub(super) async fn get_apps(&self) -> Result<String, AppFault> {
        Ok(self.apps.view(None).await?.to_string())
    }

    /// JSON: one installed app.
    pub(super) async fn get_app(&self, name: &str) -> Result<String, AppFault> {
        Ok(self.apps.view(Some(name)).await?.to_string())
    }

    /// Install or upgrade the bundle at `path`; answers the task's id.
    /// `reinstall` installs the installed version again, `upgrade` refuses
    /// an app that is not installed. Unless `accept_privileges`, a bundle
    /// that asks for privileges beyond the installed version's (any, on a
    /// first install) is refused with them. The bundle's manifest is checked
    /// before the answer.
    pub(super) async fn install_app(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        path: &str,
        reinstall: bool,
        upgrade: bool,
        accept_privileges: bool,
    ) -> Result<String, AppFault> {
        let options = crate::apps::InstallOptions {
            reinstall,
            upgrade,
            unattended: false,
            ask_privileges: !accept_privileges,
        };
        Ok(self
            .install_app_now(&emitter, sender_of(&header), path, options)
            .await?)
    }

    /// Switch an app to its previous version; answers the task's id.
    pub(super) async fn rollback_app(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        name: &str,
    ) -> Result<String, AppFault> {
        let owned = name.to_string();
        Ok(self
            .app_flow_now(
                &emitter,
                sender_of(&header),
                "app.rollback",
                name,
                |apps, _| async move { apps.rollback(&owned).await },
            )
            .await?)
    }

    /// Remove an app; `purge` deletes its data too. Answers the task's id.
    pub(super) async fn remove_app(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        name: &str,
        purge: bool,
    ) -> Result<String, AppFault> {
        let owned = name.to_string();
        Ok(self
            .app_flow_now(
                &emitter,
                sender_of(&header),
                "app.remove",
                name,
                |apps, _| async move { apps.remove(&owned, purge).await },
            )
            .await?)
    }

    /// Read an app's version file now; answers the task's id.
    pub(super) async fn check_app_update(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        name: &str,
    ) -> Result<String, AppFault> {
        let owned = name.to_string();
        Ok(self
            .app_flow_now(
                &emitter,
                sender_of(&header),
                "app.check",
                name,
                |apps, _| async move { apps.check_update(&owned).await },
            )
            .await?)
    }

    /// Fetch and install the newer version an app's version file offers;
    /// answers the task's id. A version that asks for privileges beyond the
    /// installed one's fails the task and is kept with what it asks for
    /// (`update.privileges` in `GetApp`), unless it is `accept`: the version
    /// asked about, with the same bundle and nothing more asked. `""`
    /// accepts nothing.
    pub(super) async fn update_app(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(header)] header: Header<'_>,
        name: &str,
        accept: &str,
    ) -> Result<String, AppFault> {
        let owned = name.to_string();
        let accept = accept.to_string();
        Ok(self
            .app_flow_now(
                &emitter,
                sender_of(&header),
                "app.update",
                name,
                |apps, services| async move { apps.update(&owned, &services, &accept).await },
            )
            .await?)
    }

    /// Start every unit of an app.
    pub(super) async fn start_app(&self, name: &str) -> Result<(), AppFault> {
        Ok(self.apps.act(name, crate::user_units::Verb::Start).await?)
    }

    /// Stop them.
    pub(super) async fn stop_app(&self, name: &str) -> Result<(), AppFault> {
        Ok(self.apps.act(name, crate::user_units::Verb::Stop).await?)
    }

    /// Restart them.
    pub(super) async fn restart_app(&self, name: &str) -> Result<(), AppFault> {
        Ok(self
            .apps
            .act(name, crate::user_units::Verb::Restart)
            .await?)
    }

    /// JSON: an app's units' newest log lines and its last first-run output.
    pub(super) async fn get_app_log(&self, name: &str) -> Result<String, AppFault> {
        Ok(self.apps.logs(name, self.logs.as_ref()).await?.to_string())
    }

    /// Scan for WiFi networks on the station's radio and answer what it found.
    ///
    /// The interface is the one `wifi.client` declares -- a scan is a thing
    /// the station role does, not an arbitrary radio operation, so there is no
    /// interface argument to point somewhere else.
    pub(super) async fn scan_wifi(&self) -> Result<String, SettingsFault> {
        self.scan_wifi_now().await
    }

    /// The declared Bluetooth trust list, what BlueZ reports, the pairing window
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

    /// Open the pairing window for [`crate::bluetooth::PAIR_WINDOW`], or open
    /// it afresh; answers when it closes, in seconds since the epoch. Its
    /// timer closes it again, and refuses whatever is still waiting.
    pub(super) async fn open_bluetooth_pair_window(&self) -> fdo::Result<u64> {
        self.require_bluetooth().await?;
        self.bluetooth
            .set_pair_window(true)
            .await
            .map_err(|err| fdo::Error::Failed(format!("open the pairing window: {err:#}")))?;
        let window = self.agent.open_window().await;
        let agent = Arc::clone(&self.agent);
        let bluetooth = Arc::clone(&self.bluetooth);
        tokio::spawn(async move {
            tokio::time::sleep(crate::bluetooth::PAIR_WINDOW).await;
            if agent.expire(window).await
                && let Err(err) = bluetooth.set_pair_window(false).await
            {
                tracing::warn!(error = %format!("{err:#}"), "bluetooth: the pairing window could not be closed");
            }
        });
        Ok(window.expires_at)
    }

    /// Close the pairing window now, and refuse whatever is still waiting.
    pub(super) async fn close_bluetooth_pair_window(&self) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        self.agent.close_window().await;
        self.bluetooth
            .set_pair_window(false)
            .await
            .map_err(|err| fdo::Error::Failed(format!("close the pairing window: {err:#}")))
    }

    /// Answer the passkey the agent is holding.
    pub(super) async fn confirm_bluetooth_pairing(
        &self,
        address: &str,
        accept: bool,
    ) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        if !self.agent.answer(address, accept).await {
            return Err(fdo::Error::Failed(format!(
                "no pairing confirmation is waiting for {address}"
            )));
        }
        // A pairing the peer started reaches no `PairBluetoothDevice`, so the
        // yes is where it is declared.
        if accept {
            self.declare_paired_device(address).await?;
        }
        Ok(())
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

    /// Set a declared device's `flag`, `trusted` or `blocked`, to `on`, and
    /// bring the adapter to it.
    pub(super) async fn set_bluetooth_device_flag(
        &self,
        address: &str,
        flag: &str,
        on: bool,
    ) -> fdo::Result<()> {
        self.require_feature(micad_settings::Feature::Bluetooth)?;
        let _apply = self.apply_lock.lock().await;
        let mut devices = self.inner.read().await.settings.bluetooth.devices.clone();
        let Some(device) = devices.get_mut(address) else {
            return Err(fdo::Error::InvalidArgs(format!(
                "no device named {address:?} is declared on this device"
            )));
        };
        match flag {
            "trusted" => device.trusted = on,
            "blocked" => device.blocked = on,
            _ => {
                return Err(fdo::Error::InvalidArgs(format!(
                    "{flag:?} is not a device flag: `trusted` or `blocked`"
                )));
            }
        }
        let value = serde_json::to_value(&devices).unwrap_or(Value::Null);
        self.persist_setting("bluetooth.devices", value)
            .await
            .map_err(|err| fdo::Error::Failed(format!("persist the trust list: {err}")))?;
        drop(_apply);
        self.apply_subtree("bluetooth").await;
        Ok(())
    }

    /// Start a declared container, through mica-containerd:
    /// `<project>/<service>`, or `<project>` for every container of it.
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

    /// JSON: this device's code and its source, `{code, source}`, or
    /// `{code: null, reason}` when it has none (`docs/factory-ap-key.md`).
    /// Never the access-point key or the product secret.
    pub(super) async fn get_device_code(&self) -> String {
        let code = match &self.device_root {
            Some(root) => crate::device_code::device_code(root),
            None => Err("this daemon reads no device".to_string()),
        };
        match code {
            Ok(code) => code.to_json(),
            Err(reason) => serde_json::json!({ "code": null, "reason": reason }),
        }
        .to_string()
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
        // The id of whatever is staged: a deployment's descriptor, or a set
        // under its own name.
        let verified = self.update.verified_dir();
        let core = verified.join(deployment::staged_set_name(
            deployment::CORE_KIND,
            deployment_id,
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

    /// Check for, fetch and install the newest release on a background task.
    ///
    /// Answers the refusals that hold before anything runs; the progress and
    /// the outcome land in `update` as a manual check, fetch and install do.
    pub(super) async fn install_latest_update(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> fdo::Result<()> {
        let service = server
            .interface::<_, MicadService>(super::OBJECT_PATH)
            .await
            .map_err(|err| fdo::Error::Failed(format!("the served object: {err}")))?;
        super::install_latest(
            Arc::clone(&self.update),
            service,
            sender_of(&header).to_string(),
        )
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

    /// Emitted after a successful `SetSettings`, and once per path of a
    /// `SetSettingsMany`, with the changed dot-path and its new JSON-encoded
    /// value.
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
