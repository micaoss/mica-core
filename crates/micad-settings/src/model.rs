//! The typed settings tree and its dot-path accessors.
//!
//! **There is no tree-wide `schema_version` any more.** Each document carries
//! its own, because a namespace-wide version could not be bumped without
//! rewriting every document across renames that have no transaction between
//! them.

use crate::error::SettingsError;
use crate::path::{json_path_get, json_path_set, split_path};
use serde_json::Value;
use std::collections::BTreeMap;

mod access;
mod bluetooth;
mod containers;
mod mqtt;
mod network;
mod provisioning;
mod time;
mod wifi;
pub use access::*;
pub use bluetooth::*;
pub use containers::*;
pub use mqtt::*;
pub use network::*;
pub use provisioning::*;
pub use time::*;
pub use wifi::*;

/// Persistent micad settings tree, as every reader addresses it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// System hostname.
    pub hostname: String,
    /// Per-interface network configuration, keyed by interface name.
    pub network: BTreeMap<String, IfaceSettings>,
    /// Access control settings.
    #[serde(default)]
    pub access: AccessSettings,
    /// First-boot self-provisioning status.
    #[serde(default)]
    pub provisioning: ProvisioningSettings,
    /// WiFi station and access-point settings.
    #[serde(default)]
    pub wifi: WifiSettings,
    /// Container engine policy.
    #[serde(default)]
    pub container: ContainerSettings,
    /// MQTT broker and bridge policy.
    #[serde(default)]
    pub mqtt: MqttSettings,
    /// Bluetooth adapter policy and the devices this device trusts.
    #[serde(default)]
    pub bluetooth: BluetoothSettings,
    /// NTP server and presentation-timezone settings.
    #[serde(default)]
    pub time: TimeSettings,
    /// A staged reset intent (schema v12); absent unless one is waiting to be
    /// applied — see [`ResetSettings`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hostname: "mica".to_string(),
            network: BTreeMap::new(),
            access: AccessSettings::default(),
            provisioning: ProvisioningSettings::default(),
            wifi: WifiSettings::default(),
            container: ContainerSettings::default(),
            bluetooth: BluetoothSettings::default(),
            mqtt: MqttSettings::default(),
            time: TimeSettings::default(),
            reset: None,
        }
    }
}

impl Settings {
    /// Read the node at `path` as JSON. `""` or `"."` return the whole tree.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError::NotFound`] when the path does not resolve.
    pub fn get(&self, path: &str) -> Result<Value, SettingsError> {
        let root = self.to_json()?;
        json_path_get(&root, path)
            .cloned()
            .ok_or_else(|| SettingsError::NotFound(path.to_string()))
    }

    /// Write `value` at `path`. `""` or `"."` replace the whole tree.
    ///
    /// Missing intermediate map entries are created (e.g. setting
    /// `network.eth1.dhcp` creates `eth1`), provided the resulting tree still
    /// deserializes into a valid [`Settings`]. On any error the settings are
    /// left unchanged.
    pub fn set(&mut self, path: &str, value: Value) -> Result<(), SettingsError> {
        let mut root = self.to_json()?;
        if path.is_empty() || path == "." {
            root = value;
        } else {
            let segments = split_path(path)?;
            json_path_set(&mut root, &segments, value)?;
        }
        let candidate: Self =
            serde_json::from_value(root).map_err(|err| SettingsError::Validation {
                path: path.to_string(),
                message: err.to_string(),
            })?;
        // Key-charset validation is a property of the write, not of the tree:
        // a document that already loads keeps loading, so an entry this write
        // does not touch is left alone even if a hand edit spelled it badly.
        for (iface, settings) in &candidate.network {
            if self.network.get(iface) == Some(settings) {
                continue;
            }
            crate::check_iface_name("network interface", iface).map_err(|message| {
                SettingsError::Validation {
                    path: path.to_string(),
                    message,
                }
            })?;
        }
        // The same rule again, for the containers: a document that already
        // loads keeps loading, and only a write that CHANGES the map has to
        // satisfy its predicates.
        // The same write-scoped rule again: a document that already loads
        // keeps loading, and only a write that CHANGES the subtree has to
        // satisfy its predicates.
        if candidate.bluetooth != self.bluetooth {
            validate_bluetooth(&candidate.bluetooth).map_err(|message| {
                SettingsError::Validation {
                    path: path.to_string(),
                    message,
                }
            })?;
        }
        if candidate.container.units != self.container.units {
            validate_container_units(&candidate.container.units).map_err(|message| {
                SettingsError::Validation {
                    path: path.to_string(),
                    message,
                }
            })?;
        }
        // Same rule as the network keys: a property of the write, not of the
        // tree. A document that already loads keeps loading; only a write that
        // CHANGES the `time` subtree has to satisfy its predicates.
        if candidate.time != self.time {
            validate_time_settings(&candidate.time).map_err(|message| {
                SettingsError::Validation {
                    path: path.to_string(),
                    message,
                }
            })?;
        }
        // Only a write that changes a listener is held to the port rule, for
        // the same reason: a document that already loads keeps loading.
        if candidate.access.web != self.access.web
            || candidate.access.ssh.enabled != self.access.ssh.enabled
            || candidate.access.ssh.port != self.access.ssh.port
            || candidate.mqtt.enabled != self.mqtt.enabled
            || candidate.mqtt.listen.port != self.mqtt.listen.port
        {
            access::validate_listeners(
                &candidate.access.web,
                &candidate.access.ssh,
                &candidate.mqtt,
            )
            .map_err(|message| SettingsError::Validation {
                path: path.to_string(),
                message,
            })?;
        }
        *self = candidate;
        Ok(())
    }

    fn to_json(&self) -> Result<Value, SettingsError> {
        serde_json::to_value(self).map_err(|err| SettingsError::Parse(err.to_string()))
    }
}

#[cfg(test)]
mod tests;
