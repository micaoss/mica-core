//! The documents the settings tree is stored as.
//!
//! **Storage is split; addressing is not.** [`crate::Settings`] is the one
//! tree every reader and every dot-path still sees. Underneath it, what an
//! integrator sets lives in `/mica/config/` as one flat JSON document per
//! reconciler, and what the device mints or observes about itself — the
//! identity record, the credential material derived from it, and a staged
//! reset intent — stays on STATE. [`crate::Store`] is the only thing that
//! converts between the two shapes.

use std::collections::BTreeMap;

use crate::model::{
    AccessSettings, ApiToken, BluetoothSettings, ClaimSettings, ConsoleSettings, ContainerSettings,
    DeviceCredentialSettings, DnsSettings, IfaceSettings, LanDiscoverySettings, MqttSettings,
    ProvisioningSettings, ResetSettings, ServicesSettings, Settings, SshSettings, TimeSettings,
    WebAdminSettings, WebSettings, WifiSettings,
};

/// Directory the `/mica/config/` documents live in when nothing relocates it.
pub const DEFAULT_CONFIG_DIR: &str = "/mica/config";

/// Mode every document is written at, set before the rename.
pub const DOCUMENT_MODE: u32 = 0o600;

/// The namespace's own index: the documents this schema writes, in the order
/// the table names them.
pub const CONFIG_DOCUMENTS: [&str; 10] = [
    SYSTEM_DOCUMENT,
    NETWORK_DOCUMENT,
    DNS_DOCUMENT,
    WIFI_DOCUMENT,
    SSH_DOCUMENT,
    MQTT_DOCUMENT,
    TIME_DOCUMENT,
    CONTAINER_DOCUMENT,
    BLUETOOTH_DOCUMENT,
    SERVICES_DOCUMENT,
];

/// The addressed-tree subtrees each `/mica/config/` document carries.
///
/// The document mapping in the form a caller can compute with, rather than as
/// prose a reader has to re-derive. Two callers need it and both are the
/// pour's:
pub const DOCUMENT_SUBTREES: [(&str, &[&str]); 10] = [
    (
        SYSTEM_DOCUMENT,
        &[
            "hostname",
            "access.console",
            "access.web",
            "access.mdns",
            "access.ssdp",
        ],
    ),
    (NETWORK_DOCUMENT, &["network"]),
    (DNS_DOCUMENT, &["dns"]),
    (WIFI_DOCUMENT, &["wifi"]),
    (SSH_DOCUMENT, &["access.ssh"]),
    (MQTT_DOCUMENT, &["mqtt"]),
    (TIME_DOCUMENT, &["time"]),
    (CONTAINER_DOCUMENT, &["container"]),
    (BLUETOOTH_DOCUMENT, &["bluetooth"]),
    (SERVICES_DOCUMENT, &["services"]),
];

/// The subtrees `document` carries, or `&[]` for a name that is not a
/// `/mica/config/` document of this schema.
///
/// Empty rather than a panic: the STATE remainder is read through the same
/// loader and is legitimately not in the table.
#[must_use]
pub fn document_subtrees(document: &str) -> &'static [&'static str] {
    DOCUMENT_SUBTREES
        .iter()
        .find(|(name, _)| *name == document)
        .map_or(&[], |(_, subtrees)| *subtrees)
}

/// `hostname`, `access.console` and `access.web`; the hostname and web
/// reconcilers.
pub const SYSTEM_DOCUMENT: &str = "system.json";
/// The `network` subtree; the network reconciler.
pub const NETWORK_DOCUMENT: &str = "network.json";
/// The `dns` subtree; the dns reconciler, and the network reconciler for
/// `dns.global`.
pub const DNS_DOCUMENT: &str = "dns.json";
/// The whole `wifi` subtree; the `wifiAp` and `wifiClient` reconcilers.
pub const WIFI_DOCUMENT: &str = "wifi.json";
/// `access.ssh`; the sshd reconciler.
pub const SSH_DOCUMENT: &str = "ssh.json";
/// The `mqtt` subtree; the mqtt reconciler.
pub const MQTT_DOCUMENT: &str = "mqtt.json";
/// The `time` subtree; the time reconciler.
pub const TIME_DOCUMENT: &str = "time.json";
/// The `container` subtree; the container reconciler.
pub const CONTAINER_DOCUMENT: &str = "container.json";
/// The `bluetooth` subtree; the bluetooth reconciler.
pub const BLUETOOTH_DOCUMENT: &str = "bluetooth.json";
/// The `services` subtree; the services reconciler.
pub const SERVICES_DOCUMENT: &str = "services.json";

/// Schema version of `system.json`.
pub const SYSTEM_SCHEMA_VERSION: u32 = 1;
/// Schema version of `network.json`.
pub const NETWORK_SCHEMA_VERSION: u32 = 1;
/// Schema version of `dns.json`.
pub const DNS_SCHEMA_VERSION: u32 = 1;
/// Schema version of `wifi.json`.
pub const WIFI_SCHEMA_VERSION: u32 = 1;
/// Schema version of `ssh.json`.
pub const SSH_SCHEMA_VERSION: u32 = 1;
/// Schema version of `mqtt.json`.
pub const MQTT_SCHEMA_VERSION: u32 = 1;
/// Schema version of `time.json`.
pub const TIME_SCHEMA_VERSION: u32 = 1;
/// Schema version of `container.json`.
pub const CONTAINER_SCHEMA_VERSION: u32 = 1;
/// Schema version of `bluetooth.json`.
pub const BLUETOOTH_SCHEMA_VERSION: u32 = 1;
/// Schema version of `services.json`.
pub const SERVICES_SCHEMA_VERSION: u32 = 1;
/// Schema version of the STATE document.
///
/// The remainder is a document too and takes the same rule: it is not exempt
/// from the per-document version for being what is left over.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// `system.json`: the hostname, the local console policy, where apid
/// listens and whether the device announces itself on the LAN.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// System hostname; `hostname` in the addressed tree.
    pub hostname: String,
    /// `access.console` in the addressed tree.
    #[serde(default)]
    pub console: ConsoleSettings,
    /// `access.web` in the addressed tree. Not written while it is the
    /// default.
    #[serde(default, skip_serializing_if = "WebSettings::is_default")]
    pub web: WebSettings,
    /// `access.mdns` in the addressed tree. Not written while unset.
    #[serde(default, skip_serializing_if = "LanDiscoverySettings::is_unset")]
    pub mdns: LanDiscoverySettings,
    /// `access.ssdp` in the addressed tree. Not written while unset.
    #[serde(default, skip_serializing_if = "LanDiscoverySettings::is_unset")]
    pub ssdp: LanDiscoverySettings,
}

/// `network.json`: the per-interface network configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `network` in the addressed tree, keyed by interface name.
    #[serde(default)]
    pub network: BTreeMap<String, IfaceSettings>,
}

/// `dns.json`: the global DNS servers and the static host records.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DnsDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `dns` in the addressed tree.
    #[serde(default)]
    pub dns: DnsSettings,
}

/// `wifi.json`: the station and access-point settings, and the site's WPA2
/// key.
///
/// This is the document the namespace's `0600` exists for.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `wifi` in the addressed tree.
    #[serde(default)]
    pub wifi: WifiSettings,
}

/// `ssh.json`: the SSH channel policy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `access.ssh` in the addressed tree.
    #[serde(default)]
    pub ssh: SshSettings,
}

/// `mqtt.json`: the broker and bridge policy.
///
/// Policy only: the broker reads its accounts from a STATE file rather than
/// from `mqtt.auth`, which is the rule already stated at the field — the tree
/// carries the policy, the STATE file carries the secret.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `mqtt` in the addressed tree.
    #[serde(default)]
    pub mqtt: MqttSettings,
}

/// `time.json`: the managed NTP servers and the presentation timezone.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `time` in the addressed tree.
    #[serde(default)]
    pub time: TimeSettings,
}

/// `container.json`: the container engine policy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `container` in the addressed tree, but its projects.
    #[serde(default)]
    pub container: ContainerPolicy,
}

/// `container` as `container.json` holds it: the projects are files of their
/// own ([`CONTAINER_PROJECTS_DIR`]).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerPolicy {
    /// `container.enabled`.
    pub enabled: bool,
    /// `container.projectDirs`.
    #[serde(rename = "projectDirs", skip_serializing_if = "BTreeMap::is_empty")]
    pub project_dirs: BTreeMap<String, String>,
}

/// The directory in `/mica/config/` holding one file per Compose project,
/// `<project>.yaml`: `container.projects` in the addressed tree.
pub const CONTAINER_PROJECTS_DIR: &str = "container";
/// The suffix of a project's file.
pub const CONTAINER_PROJECT_SUFFIX: &str = ".yaml";

/// The document name of `project`'s file, as the store reports and journals
/// it: `container/<project>.yaml`.
#[must_use]
pub fn container_project_document(project: &str) -> String {
    format!("{CONTAINER_PROJECTS_DIR}/{project}{CONTAINER_PROJECT_SUFFIX}")
}

/// The project a document name is the file of, when it is one.
#[must_use]
pub fn container_project_of(document: &str) -> Option<&str> {
    document
        .strip_prefix(CONTAINER_PROJECTS_DIR)?
        .strip_prefix('/')?
        .strip_suffix(CONTAINER_PROJECT_SUFFIX)
        .filter(|name| crate::model::is_project_name(name))
}

/// `bluetooth.json`: the adapter policy and the devices this device trusts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BluetoothDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `bluetooth` in the addressed tree.
    #[serde(default)]
    pub bluetooth: BluetoothSettings,
}

/// `services.json`: the user's own services, each unit's text in it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicesDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `services` in the addressed tree.
    #[serde(default)]
    pub services: ServicesSettings,
}

/// The half of `access` that stays on STATE.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StateAccessSettings {
    /// Web admin credentials; absent until apid sets them.
    #[serde(rename = "webAdmin", skip_serializing_if = "Option::is_none")]
    pub web_admin: Option<WebAdminSettings>,
    /// How this device was claimed; absent until it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimSettings>,
    /// Device credential metadata; never holds a plaintext secret.
    pub device: DeviceCredentialSettings,
    /// Bearer API tokens, hashes only.
    ///
    /// Declared last so the TOML serializer emits this array of tables after
    /// every other key of `access`.
    #[serde(rename = "apiTokens", skip_serializing_if = "Vec::is_empty")]
    pub api_tokens: Vec<ApiToken>,
}

/// The STATE document: what the device mints or observes about itself, the
/// credential material derived from it, and the intents it is carrying out.
///
/// Still TOML at `/var/lib/mica/settings.toml`. The JSON rule is the
/// `/mica/config/` namespace's, and this document is not in it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDocument {
    /// Version of this document alone.
    pub schema_version: u32,
    /// `provisioning` in the addressed tree.
    #[serde(default)]
    pub provisioning: ProvisioningSettings,
    /// The half of `access` that is not configuration.
    #[serde(default)]
    pub access: StateAccessSettings,
    /// A staged reset intent; absent unless one is waiting to be applied.
    ///
    /// Declared last so the TOML serializer emits this table after every other
    /// key of the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetSettings>,
}

/// Every document one device holds, as one value.
///
/// The composition and the split are both total and both live here, so a field
/// that is added to [`Settings`] and forgotten in one direction does not
/// compile.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DocumentSet {
    pub system: SystemDocument,
    pub network: NetworkDocument,
    pub dns: DnsDocument,
    pub wifi: WifiDocument,
    pub ssh: SshDocument,
    pub mqtt: MqttDocument,
    pub time: TimeDocument,
    pub container: ContainerDocument,
    pub projects: BTreeMap<String, String>,
    pub bluetooth: BluetoothDocument,
    pub services: ServicesDocument,
    pub state: StateDocument,
}

impl DocumentSet {
    /// Split `settings` into the documents that store it.
    pub(crate) fn of(settings: &Settings) -> Self {
        Self {
            system: SystemDocument {
                schema_version: SYSTEM_SCHEMA_VERSION,
                hostname: settings.hostname.clone(),
                console: settings.access.console.clone(),
                web: settings.access.web,
                mdns: settings.access.mdns,
                ssdp: settings.access.ssdp,
            },
            network: NetworkDocument {
                schema_version: NETWORK_SCHEMA_VERSION,
                network: settings.network.clone(),
            },
            dns: DnsDocument {
                schema_version: DNS_SCHEMA_VERSION,
                dns: settings.dns.clone(),
            },
            wifi: WifiDocument {
                schema_version: WIFI_SCHEMA_VERSION,
                wifi: settings.wifi.clone(),
            },
            ssh: SshDocument {
                schema_version: SSH_SCHEMA_VERSION,
                ssh: settings.access.ssh.clone(),
            },
            mqtt: MqttDocument {
                schema_version: MQTT_SCHEMA_VERSION,
                mqtt: settings.mqtt.clone(),
            },
            time: TimeDocument {
                schema_version: TIME_SCHEMA_VERSION,
                time: settings.time.clone(),
            },
            container: ContainerDocument {
                schema_version: CONTAINER_SCHEMA_VERSION,
                container: ContainerPolicy {
                    enabled: settings.container.enabled,
                    project_dirs: settings.container.project_dirs.clone(),
                },
            },
            projects: settings.container.projects.clone(),
            bluetooth: BluetoothDocument {
                schema_version: BLUETOOTH_SCHEMA_VERSION,
                bluetooth: settings.bluetooth.clone(),
            },
            services: ServicesDocument {
                schema_version: SERVICES_SCHEMA_VERSION,
                services: settings.services.clone(),
            },
            state: StateDocument {
                schema_version: STATE_SCHEMA_VERSION,
                provisioning: settings.provisioning.clone(),
                access: StateAccessSettings {
                    web_admin: settings.access.web_admin.clone(),
                    claim: settings.access.claim,
                    device: settings.access.device.clone(),
                    api_tokens: settings.access.api_tokens.clone(),
                },
                reset: settings.reset.clone(),
            },
        }
    }

    /// Compose the documents back into the one addressed tree.
    pub(crate) fn compose(self) -> Settings {
        Settings {
            hostname: self.system.hostname,
            network: self.network.network,
            dns: self.dns.dns,
            access: AccessSettings {
                web_admin: self.state.access.web_admin,
                claim: self.state.access.claim,
                ssh: self.ssh.ssh,
                console: self.system.console,
                web: self.system.web,
                mdns: self.system.mdns,
                ssdp: self.system.ssdp,
                device: self.state.access.device,
                api_tokens: self.state.access.api_tokens,
            },
            provisioning: self.state.provisioning,
            wifi: self.wifi.wifi,
            container: ContainerSettings {
                enabled: self.container.container.enabled,
                projects: self.projects,
                project_dirs: self.container.container.project_dirs,
            },
            bluetooth: self.bluetooth.bluetooth,
            mqtt: self.mqtt.mqtt,
            time: self.time.time,
            services: self.services.services,
            reset: self.state.reset,
        }
    }
}

/// The document set a device with no stored document at all has.
///
/// This is the only place the "a missing document is its schema default"
/// rule is expressed, and it is expressed by splitting [`Settings::default`]
/// rather than by restating any default — so a subtree added to the schema
/// arrives here with the value its own type declares.
impl Default for DocumentSet {
    fn default() -> Self {
        Self::of(&Settings::default())
    }
}

macro_rules! document_default {
    ($document:ty, $field:ident) => {
        impl Default for $document {
            fn default() -> Self {
                DocumentSet::default().$field
            }
        }
    };
}

document_default!(SystemDocument, system);
document_default!(NetworkDocument, network);
document_default!(DnsDocument, dns);
document_default!(WifiDocument, wifi);
document_default!(SshDocument, ssh);
document_default!(MqttDocument, mqtt);
document_default!(TimeDocument, time);
document_default!(ContainerDocument, container);
document_default!(BluetoothDocument, bluetooth);
document_default!(ServicesDocument, services);
document_default!(StateDocument, state);

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace's index and the subtree table are the same list read two
    /// ways. A document added to one and forgotten in the other would leave
    /// the pour's refusal path with a document it cannot name a subsystem for
    /// — which fails open, because a refusal that names no subtree refuses
    /// nothing.
    #[test]
    fn every_config_document_declares_its_subtrees() {
        let declared: Vec<&str> = DOCUMENT_SUBTREES.iter().map(|(name, _)| *name).collect();
        assert_eq!(declared, CONFIG_DOCUMENTS.to_vec());
        for (name, subtrees) in DOCUMENT_SUBTREES {
            assert!(!subtrees.is_empty(), "{name} declares no subtree");
            assert_eq!(document_subtrees(name), subtrees);
        }
    }

    /// The STATE remainder is read through the same loader and is not in the
    /// namespace, so the lookup has to answer for it without pretending it is
    /// a `/mica/config/` document.
    #[test]
    fn a_name_outside_the_namespace_declares_no_subtree() {
        assert!(document_subtrees("settings.toml").is_empty());
        assert!(document_subtrees("updates.json").is_empty());
    }
}
