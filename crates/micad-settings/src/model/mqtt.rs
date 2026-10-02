//! The MQTT broker and bridge settings.

/// MQTT policy: the master switch for the broker and the bridge, and the
/// listener and credential policy the broker is rendered from.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttSettings {
    /// Whether the broker and the bridge run at all.
    pub enabled: bool,
    /// Where the broker listens.
    pub listen: MqttListenSettings,
    /// Whether the broker demands credentials.
    pub auth: MqttAuthSettings,
}

/// Where the broker listens.
///
/// Loopback and the MQTT default port: the bridge is an on-device client, so
/// the reachable-by-default listener a wider bind would create is one nobody
/// asked for. Widening it is a deliberate operator edit, and -- see
/// [`MqttSettings`] -- nothing refuses to start because of what is here.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttListenSettings {
    /// Address the broker binds.
    pub address: String,
    /// TCP port the broker listens on.
    pub port: u16,
}

impl Default for MqttListenSettings {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".to_string(),
            port: 1883,
        }
    }
}

/// Whether the broker demands credentials from a connecting client.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttAuthSettings {
    /// Whether a client must authenticate to connect.
    pub enabled: bool,
}
