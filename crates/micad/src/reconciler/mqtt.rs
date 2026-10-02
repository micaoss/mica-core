//! MQTT reconciler: one master switch driving the broker and the bridge that
//! needs it, and the broker's runtime config rendered from `mqtt`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use micad_settings::{MqttSettings, Settings};
use serde_json::json;

use super::Reconciler;
use super::systemd::{UnitControl, is_active, is_enabled};
use crate::fswrite::write_config_if_changed;

/// Unit implementing the MQTT broker (rumqttd, used as a library).
const BROKER_UNIT: &str = "mica-mqtt-broker.service";
/// Unit implementing the bridge from the broker to the cloud. Predates this
/// reconciler; unchanged by it apart from who starts and stops it.
const BRIDGE_UNIT: &str = "mica-mqttd.service";
/// Config the broker binary reads, rendered by micad at runtime.
///
/// On `/run`, not on STATE: it is derived entirely from the settings tree and
/// is re-rendered on every boot before the broker starts, so persisting it
/// would only create a second copy of the truth that could disagree with the
/// first.
const DEFAULT_CONFIG_PATH: &str = "/run/mica/mqtt-broker.toml";
/// Root-rendered runtime identity read by `mica-mqttd.service`.
const IDENTITY_FILE_NAME: &str = "mqttd-device.env";
/// Fixed production path required by `mica-mqttd.service`.
const DEFAULT_IDENTITY_PATH: &str = "/run/mica/mqttd-device.env";
/// Environment variable overriding the rendered config path.
///
/// Nothing in the image sets it; the override exists so tests run entirely
/// inside a temporary directory and never touch the host's `/run`.
const CONFIG_PATH_ENV: &str = "MICAD_MQTT_BROKER_CONFIG";
/// Mode of the rendered config: world-readable, owner-writable.
const CONFIG_MODE: u32 = 0o644;
/// `ActiveState` of a unit systemd has given up on.
const FAILED_STATE: &str = "failed";

/// Reconciler for the `mqtt` settings subtree.
pub struct MqttReconciler<C: UnitControl> {
    /// Path the broker config is rendered to.
    config_path: PathBuf,
    /// One-purpose runtime identity file beside the broker configuration.
    identity_path: PathBuf,
    control: C,
}

impl<C: UnitControl> MqttReconciler<C> {
    /// Create an MQTT reconciler rendering the broker config to `config_path`
    /// and driving both units through `control`.
    ///
    /// The path is a parameter so tests run entirely inside a temporary
    /// directory and never touch the host's `/run`.
    pub fn new(config_path: PathBuf, control: C) -> Self {
        let identity_path = config_path
            .parent()
            .map(|parent| parent.join(IDENTITY_FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(IDENTITY_FILE_NAME));
        Self {
            config_path,
            identity_path,
            control,
        }
    }
}

impl<C: UnitControl> MqttReconciler<C> {
    /// Production reconciler driving the services through `control`: config
    /// path from [`CONFIG_PATH_ENV`] if set, else [`DEFAULT_CONFIG_PATH`].
    pub fn production(control: C) -> Self {
        let config_path = std::env::var(CONFIG_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH));
        let mut reconciler = Self::new(config_path, control);
        // The broker-config override exists for tests and diagnostics, but the
        // systemd unit intentionally names one fixed identity path. Never let
        // an unrelated broker path move this security boundary.
        reconciler.identity_path = PathBuf::from(DEFAULT_IDENTITY_PATH);
        reconciler
    }
}

/// How `mqtt.listen.address` reads to the broker.
#[derive(Debug, PartialEq, Eq)]
enum ListenAddress {
    /// Parses as an `IpAddr` and is loopback: reachable only from the device.
    Loopback,
    /// Parses as an `IpAddr` and is not loopback: reachable from the network.
    OffHost,
    /// Does not parse as an `IpAddr`.
    ///
    /// `mica-mqtt-broker` parses `listen_address` as an `IpAddr` and does not
    /// resolve names, so a value like `"localhost"` is a startup error and the
    /// process exits. Nothing here refuses anything for it — see
    /// [`MqttReconciler::apply`].
    Unparseable,
}

/// Classify `address` for the warnings in [`MqttReconciler::apply`].
///
/// Pure: it decides what to say, never whether to act.
fn classify_listen_address(address: &str) -> ListenAddress {
    match address.parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_loopback() => ListenAddress::Loopback,
        Ok(_) => ListenAddress::OffHost,
        Err(_) => ListenAddress::Unparseable,
    }
}

/// Render the broker config for `mqtt`.
///
/// Pure and deterministic: the same settings always produce the same bytes, so
/// a re-render compared against what is on disk tells [`MqttReconciler::apply`]
/// whether a running broker has to be restarted.
fn render_config(mqtt: &MqttSettings) -> String {
    let mut out = String::new();
    out.push_str(&format!("listen_address = \"{}\"\n", mqtt.listen.address));
    out.push_str(&format!("listen_port = {}\n", mqtt.listen.port));
    out.push_str(&format!("auth_enabled = {}\n", mqtt.auth.enabled));
    out
}

/// Render the one environment assignment consumed by `mica-mqttd.service`.
fn render_device_identity(device_id: Option<&str>) -> Result<String> {
    let device_id = device_id.context("mqtt: device identity is not provisioned")?;
    if device_id.is_empty()
        || !device_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        anyhow::bail!(
            "mqtt: device identity must contain only ASCII letters, digits, '.', '_', '-' or ':'"
        );
    }
    Ok(format!("MICA_MQTT_DEVICE_ID={device_id}\n"))
}

impl<C: UnitControl> MqttReconciler<C> {
    /// Start `unit`, and on failure WARN instead of failing the reconcile.
    ///
    /// `apply` covers the whole `mqtt` subtree, so propagating a start failure
    /// would couple `mqtt.enabled` to `mqtt.listen` being valid -- the coupling
    /// this reconciler does not have (see [`micad_settings::MqttSettings`]).
    async fn start_or_warn(&self, unit: &str) {
        if let Err(error) = self.control.start(unit).await {
            tracing::warn!(
                unit,
                %error,
                "mqtt: the unit refused to start; mqtt.enabled stays applied and the unit's real \
                 state is published as it is"
            );
        }
    }

    /// Clear `unit`'s failed state so the start after it is not refused.
    async fn reset_failed_or_warn(&self, unit: &str) {
        if let Err(error) = self.control.reset_failed(unit).await {
            tracing::warn!(
                unit,
                %error,
                "mqtt: could not clear the unit's failed state; a start may be refused until its \
                 start-limit window elapses"
            );
        }
    }

    /// Bring the broker up, restarting it when the config it is running
    /// against has been rewritten.
    ///
    /// Reads before it writes, so a broker already in the target state and
    /// running against the current config gets no calls at all.
    async fn turn_broker_on(&self, config_changed: bool) -> Result<()> {
        if !is_enabled(&self.control.unit_file_state(BROKER_UNIT).await?) {
            self.control.enable(BROKER_UNIT).await?;
        }
        let active_state = self.control.active_state(BROKER_UNIT).await?;
        if is_active(&active_state) {
            if config_changed {
                self.control.restart(BROKER_UNIT).await?;
            }
        } else {
            // A failed broker may be inside its start-limit window, where
            // systemd refuses start jobs outright. Clear the failure first, so
            // an operator who has just corrected the address gets a broker that
            // comes up on this apply rather than one that stays down until the
            // minute expires.
            if active_state == FAILED_STATE {
                self.reset_failed_or_warn(BROKER_UNIT).await;
            }
            // Not running: start it. A restart here would work too, but
            // starting says what is meant, and a broker that is down is not
            // running a stale config — it is running none.
            self.start_or_warn(BROKER_UNIT).await;
        }
        Ok(())
    }

    /// Bring the bridge up, restarting it when its root-rendered identity
    /// changed. Broker configuration changes still need no bridge restart: its
    /// reconnect loop handles the broker transition.
    async fn turn_bridge_on(&self, identity_changed: bool) -> Result<()> {
        if !is_enabled(&self.control.unit_file_state(BRIDGE_UNIT).await?) {
            self.control.enable(BRIDGE_UNIT).await?;
        }
        if is_active(&self.control.active_state(BRIDGE_UNIT).await?) {
            if identity_changed {
                self.control.restart(BRIDGE_UNIT).await?;
            }
        } else {
            self.start_or_warn(BRIDGE_UNIT).await;
        }
        Ok(())
    }

    /// Stop and disable `unit` if it is not already down.
    async fn turn_unit_off(&self, unit: &str) -> Result<()> {
        if is_active(&self.control.active_state(unit).await?) {
            self.control.stop(unit).await?;
        }
        if is_enabled(&self.control.unit_file_state(unit).await?) {
            self.control.disable(unit).await?;
        }
        Ok(())
    }

    /// Live state of `unit`, read after the transition so what is published is
    /// what the system now is rather than what it was asked to become.
    async fn unit_state(&self, unit: &str) -> Result<serde_json::Value> {
        Ok(json!({
            "unit": unit,
            "activeState": self.control.active_state(unit).await?,
            "unitFileState": self.control.unit_file_state(unit).await?,
        }))
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for MqttReconciler<C> {
    fn name(&self) -> &'static str {
        "mqtt"
    }

    fn subtree(&self) -> &'static str {
        "mqtt"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let mqtt = &settings.mqtt;

        // Unconditionally, and before any unit is touched: the file then
        // describes what the switch would start even while it is off, and a
        // broker is never started against a config older than the settings
        // that were just applied.
        let config_changed =
            write_config_if_changed(&self.config_path, &render_config(mqtt), CONFIG_MODE)?;
        // Not `?`: the identity is the bridge's input and nobody else's. An
        // identity that cannot be rendered withholds the bridge below and
        // must not stop the broker, and must never stop the off path -- a
        // switch that cannot turn the units off is worse than a bridge that
        // does not start.
        let identity = render_device_identity(settings.provisioning.device_id.as_deref()).and_then(
            |rendered| write_config_if_changed(&self.identity_path, &rendered, CONFIG_MODE),
        );

        if mqtt.enabled {
            // WARNs, and deliberately not gates. Neither may become a refusal
            // and neither may skip a unit: an operator who widened the bind
            // made a decision, and a daemon that answers by quietly not
            // starting is one whose reason for being down cannot be read
            // anywhere. `listen`/`auth` are deliberately not coupled to the
            // master switch -- see `micad_settings::MqttSettings`.
            match classify_listen_address(&mqtt.listen.address) {
                // Not a wide bind -- not a bind at all. The config is rendered
                // verbatim anyway (see `apply_config`), the unit is started
                // anyway, and the broker exits with a parse error naming the
                // file and the value. That lands the unit in `failed`, which
                // the `units` array below reports, so the operator reads the
                // real cause in one place instead of two half-causes.
                ListenAddress::Unparseable => tracing::warn!(
                    address = %mqtt.listen.address,
                    "mqtt: listen address is not an IP address; the broker does not resolve names \
                     and will refuse to start against it"
                ),
                ListenAddress::OffHost if !mqtt.auth.enabled => tracing::warn!(
                    address = %mqtt.listen.address,
                    port = mqtt.listen.port,
                    "mqtt: the broker is bound off-host with authentication disabled; it accepts \
                     unauthenticated connections from the network"
                ),
                ListenAddress::Loopback | ListenAddress::OffHost => {}
            }
            // Broker first: the bridge is its client.
            self.turn_broker_on(config_changed).await?;
            match identity {
                Ok(identity_changed) => self.turn_bridge_on(identity_changed).await?,
                Err(error) => tracing::warn!(
                    %error,
                    "mqtt: the bridge identity could not be rendered; the bridge is not started \
                     and a running one keeps the identity it has"
                ),
            }
        } else {
            // Bridge first, the reverse of start: the client goes before the
            // server it talks to, so a deliberate shutdown does not read as a
            // connection failure in the bridge's journal.
            self.turn_unit_off(BRIDGE_UNIT).await?;
            self.turn_unit_off(BROKER_UNIT).await?;
        }

        // The shape below is a contract with a consumer in another crate:
        // `micad/apid/src/routes.rs` renders the MQTT pane by reading these
        // keys by name out of the bus item this becomes. Nothing in the type
        // system connects the two -- apid talks to micad over the bus -- so
        // `the_published_shape_is_the_contract_with_the_apid_pane` below
        // asserts the exact key set, and changing a key here means changing
        // the pane. Skipping that does not break loudly: the pane renders
        // "unknown", keeps its 200, and the warning it exists to raise
        // silently never fires again.
        Ok(json!({
            "enabled": mqtt.enabled,
            "listen": {
                "address": mqtt.listen.address,
                "port": mqtt.listen.port,
            },
            "auth": {
                // Policy only. There is no credential in the settings tree to
                // publish -- the broker's accounts live on STATE.
                "enabled": mqtt.auth.enabled,
            },
            "configPath": self.config_path.display().to_string(),
            "units": [
                self.unit_state(BROKER_UNIT).await?,
                self.unit_state(BRIDGE_UNIT).await?,
            ],
        }))
    }
}

#[cfg(test)]
mod tests;
