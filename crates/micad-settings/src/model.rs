//! The typed settings tree and its dot-path accessors.
//!
//! **There is no tree-wide `schema_version` any more.** Each document carries
//! its own, because a namespace-wide version could not be bumped without
//! rewriting every document across renames that have no transaction between
//! them.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::SettingsError;
use crate::path::{json_path_get, json_path_set, split_path};

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

/// Time synchronization and presentation-timezone settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeSettings {
    /// Managed NTP servers.
    pub ntp: NtpSettings,
    /// IANA timezone name used for presentation and explicitly local
    /// schedules; it never moves the machine clock off UTC.
    pub timezone: String,
}

impl Default for TimeSettings {
    fn default() -> Self {
        Self {
            ntp: NtpSettings::default(),
            timezone: "UTC".to_string(),
        }
    }
}

/// The managed NTP server list.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NtpSettings {
    /// Server names or addresses, rendered in order into timesyncd's runtime
    /// `NTP=` list. Empty means the image's fallback pool is used — an empty
    /// list is "no operator override", not "no time synchronization".
    pub servers: Vec<String>,
}

/// The most servers one `time.ntp.servers` list may carry.
///
/// timesyncd polls one selected server at a time and steps through the list
/// only on failure, so a longer list buys redundancy, not accuracy; eight is
/// well past any real deployment and keeps the rendered `NTP=` line bounded.
pub const MAX_NTP_SERVERS: usize = 8;

/// RFC 1035's bound on a full domain name, which also covers any IP literal.
const MAX_NTP_SERVER_LEN: usize = 253;

/// Longest IANA zone name accepted; the longest real one is around 32 bytes.
const MAX_TIMEZONE_LEN: usize = 64;

/// Refuse a `time.ntp.servers` list timesyncd's `NTP=` line cannot carry.
pub fn validate_ntp_servers(servers: &[String]) -> Result<(), String> {
    if servers.len() > MAX_NTP_SERVERS {
        return Err(format!(
            "at most {MAX_NTP_SERVERS} NTP servers are supported; timesyncd only ever polls one \
             and steps through the rest on failure"
        ));
    }
    for (index, server) in servers.iter().enumerate() {
        if server.is_empty() {
            return Err(format!("NTP server {} is empty", index + 1));
        }
        if server.len() > MAX_NTP_SERVER_LEN {
            return Err(format!(
                "NTP server {server:?} is longer than {MAX_NTP_SERVER_LEN} characters"
            ));
        }
        if !server
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
        {
            return Err(format!(
                "NTP server {server:?} contains a character a host name or IP address cannot \
                 have; use ASCII letters, digits, '.', '-' or ':'"
            ));
        }
        if servers[..index].contains(server) {
            return Err(format!("NTP server {server:?} is listed twice"));
        }
    }
    Ok(())
}

/// Refuse a `time.timezone` value that is not an IANA zone name.
pub fn validate_timezone_name(zone: &str) -> Result<(), String> {
    const RULES: &str = "a timezone is an IANA zone name such as \"UTC\" or \"Europe/Berlin\": \
                         '/'-separated components of ASCII letters, digits, '.', '_', '+' and '-'";
    if zone.is_empty() || zone.len() > MAX_TIMEZONE_LEN {
        return Err(format!(
            "{RULES}, between 1 and {MAX_TIMEZONE_LEN} characters"
        ));
    }
    for component in zone.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(RULES.to_string());
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
        {
            return Err(RULES.to_string());
        }
    }
    Ok(())
}

/// The `time` subtree's whole write rule, called by [`Settings::set`].
fn validate_time_settings(time: &TimeSettings) -> Result<(), String> {
    validate_ntp_servers(&time.ntp.servers)?;
    validate_timezone_name(&time.timezone)
}

/// Container engine policy, reconciled by `ContainerReconciler`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerSettings {
    /// Whether the Quadlet directory is bound from STATE and container units
    /// may run.
    pub enabled: bool,
    /// The containers this device runs, by name.
    ///
    /// Empty and skipped when empty, so a device that declares none writes the
    /// document it wrote before containers could be declared at all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, ContainerUnit>,
}

/// One container, as the fields a Quadlet `.container` unit needs.
///
/// A deliberately small subset of Quadlet's surface: enough to declare a
/// container, and no field the renderer would have to keep honest forever for
/// nobody. Unknown keys are refused like everywhere else.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerUnit {
    /// The image reference, tag included.
    pub image: String,
    /// The command to run instead of the image's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Environment variables passed into the container.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Ports published from the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<PublishedPort>,
    /// Host paths mounted into the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeMount>,
    /// What systemd does when the container exits.
    #[serde(default)]
    pub restart: RestartPolicy,
    /// Whether the unit starts at boot.
    #[serde(rename = "autoStart", default)]
    pub auto_start: bool,
}

/// One published port: a host port, the container port behind it, and the
/// protocol.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedPort {
    /// The port on the host.
    pub host: u16,
    /// The port inside the container.
    pub container: u16,
    /// `tcp` or `udp`.
    #[serde(default)]
    pub protocol: PortProtocol,
}

/// The transport a published port carries.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PortProtocol {
    /// TCP.
    #[default]
    Tcp,
    /// UDP.
    Udp,
}

impl PortProtocol {
    /// The spelling podman takes in a `PublishPort=` line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One bind mount from the device into the container.
///
/// **The host path is bounded to `/mica/`** by [`validate_container_units`].
/// A bind of `/` or `/etc` hands the device's root filesystem to whatever the
/// image runs, and the settings file is writable without apid, so the bound is
/// stated here rather than only on the write path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMount {
    /// The path on the device.
    pub host: String,
    /// Where it appears inside the container.
    pub container: String,
    /// Whether the container sees it read-only.
    #[serde(rename = "readOnly", default)]
    pub read_only: bool,
}

/// What systemd does when a container exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Leave it stopped.
    #[default]
    No,
    /// Restart it when it fails.
    OnFailure,
    /// Restart it whenever it stops.
    Always,
}

impl RestartPolicy {
    /// The spelling systemd takes in a `Restart=` line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::OnFailure => "on-failure",
            Self::Always => "always",
        }
    }
}

/// The prefix every container volume's host path must sit under.
///
/// DATA, and nothing else. `/mica/` is the operator's half of the device;
/// everything outside it is either the signed read-only root or the management
/// plane's own state.
pub const CONTAINER_VOLUME_ROOT: &str = "/mica/";

/// The longest a container name may be.
///
/// It becomes `<name>.container` and then a unit name, and a name that cannot
/// be a unit is a container that can be declared and never started.
const MAX_CONTAINER_NAME_LEN: usize = 64;

/// Refuse a container map no device could run.
///
/// Four rules, each of them a configuration Quadlet would accept and the
/// device could not use:
///
/// - a name that is not a unit name fragment;
/// - an empty image reference;
/// - one host port published by two containers, which is two units racing for
///   the same listener;
/// - a volume whose host path is not under [`CONTAINER_VOLUME_ROOT`], or that
///   climbs out of it with `..`.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_container_units(units: &BTreeMap<String, ContainerUnit>) -> Result<(), String> {
    let mut claimed: BTreeMap<(u16, PortProtocol), &str> = BTreeMap::new();
    for (name, unit) in units {
        if name.is_empty() || name.len() > MAX_CONTAINER_NAME_LEN {
            return Err(format!(
                "container name {name:?} is empty or longer than {MAX_CONTAINER_NAME_LEN} characters"
            ));
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(format!(
                "container name {name:?} contains a character a systemd unit name cannot have; use letters, digits, `-` and `_`"
            ));
        }
        if unit.image.trim().is_empty() {
            return Err(format!("container {name:?} declares no image"));
        }
        for port in &unit.publish {
            if let Some(other) = claimed.insert((port.host, port.protocol), name) {
                return Err(format!(
                    "containers {other:?} and {name:?} both publish host port {}/{}",
                    port.host,
                    port.protocol.as_str()
                ));
            }
        }
        for volume in &unit.volumes {
            if !volume.host.starts_with(CONTAINER_VOLUME_ROOT) || volume.host.contains("..") {
                return Err(format!(
                    "container {name:?} mounts {:?}; a container volume's host path is under {CONTAINER_VOLUME_ROOT} and may not climb out of it",
                    volume.host
                ));
            }
            if !volume.container.starts_with('/') {
                return Err(format!(
                    "container {name:?} mounts {:?} at {:?}, which is not an absolute path inside the container",
                    volume.host, volume.container
                ));
            }
        }
    }
    Ok(())
}

/// Bluetooth policy: whether the adapter runs, how it presents itself, and
/// which devices it trusts.
///
/// Every field is declared. What BlueZ currently sees -- which devices are in
/// range, which are connected -- is observed and lives nowhere in this tree:
/// a paired phone that is switched off is still a declared device, and a phone
/// in range that nobody paired is not one.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BluetoothSettings {
    /// Whether `bluetooth.service` runs and the adapter is powered.
    pub enabled: bool,
    /// Whether the adapter answers a scan.
    ///
    /// Off by default: a device that is discoverable is one anybody in range
    /// can see. Pairing turns it on for as long as the operator is pairing.
    pub discoverable: bool,
    /// The name the adapter advertises; absent advertises the hostname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// The pairing code offered to a peer that asks for one.
    ///
    /// **Displayed, not hidden.** A legacy peer asks the device for a code and
    /// somebody has to type it on the peer's keypad, so this value is shown in
    /// the console by design and is not a secret. Absent derives one from the
    /// device identity: fixed for this device, stable across boots, and not
    /// the same code as every other device in the fleet -- the rule
    /// [`WifiApSettings::psk`] states, applied to the one value here that has
    /// to be readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// The devices this device has paired with, by address.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub devices: BTreeMap<String, PairedDevice>,
}

/// One paired device, as the settings tree holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PairedDevice {
    /// What it called itself when it paired; empty when it offered no name.
    pub name: String,
    /// Whether it may reconnect without being confirmed again.
    pub trusted: bool,
    /// Whether the adapter refuses it.
    pub blocked: bool,
}

/// The digits a pairing code may carry, and how many.
///
/// Bluetooth's legacy PIN is 1 to 16 characters; every keypad that will be
/// asked to enter one has digits and nothing else, so the code is digits.
const MIN_PIN_LEN: usize = 4;
const MAX_PIN_LEN: usize = 16;

/// The pairing code this device offers when none is declared.
///
/// Derived from the device identifier: the first [`MIN_PIN_LEN`] digits of its
/// hexadecimal, with each hex digit folded into a decimal one. Deterministic,
/// so the console and the agent always show and answer the same code without
/// storing it; per device, so a fleet does not share one PIN; and derived from
/// the identity rather than from a credential, because it is displayed.
#[must_use]
pub fn derived_pairing_pin(device_id: &str) -> String {
    let digits: String = device_id
        .bytes()
        .filter_map(|byte| (byte as char).to_digit(16))
        .map(|value| char::from_digit(value % 10, 10).unwrap_or('0'))
        .take(MIN_PIN_LEN)
        .collect();
    if digits.len() == MIN_PIN_LEN {
        digits
    } else {
        // A device with no identifier yet: the reconciler has nothing to
        // derive from, and a code that is shown has to be something.
        "0".repeat(MIN_PIN_LEN)
    }
}

/// Refuse a pairing code no keypad could enter.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_pairing_pin(pin: &str) -> Result<(), String> {
    if pin.len() < MIN_PIN_LEN || pin.len() > MAX_PIN_LEN {
        return Err(format!(
            "a pairing code is {MIN_PIN_LEN} to {MAX_PIN_LEN} digits"
        ));
    }
    if !pin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("a pairing code is digits: every keypad that will be asked to enter one has those and nothing else".to_string());
    }
    Ok(())
}

/// Refuse a device map no adapter could hold.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_bluetooth(settings: &BluetoothSettings) -> Result<(), String> {
    if let Some(pin) = &settings.pin {
        validate_pairing_pin(pin)?;
    }
    if let Some(alias) = &settings.alias
        && (alias.is_empty() || alias.len() > 64)
    {
        return Err("a Bluetooth alias is 1 to 64 characters".to_string());
    }
    for address in settings.devices.keys() {
        if !is_bluetooth_address(address) {
            return Err(format!(
                "{address:?} is not a Bluetooth address: six hexadecimal octets separated by colons, such as `AA:BB:CC:DD:EE:FF`"
            ));
        }
    }
    Ok(())
}

/// Whether `value` is the `AA:BB:CC:DD:EE:FF` an adapter names a device by.
#[must_use]
pub fn is_bluetooth_address(value: &str) -> bool {
    let octets: Vec<&str> = value.split(':').collect();
    octets.len() == 6
        && octets
            .iter()
            .all(|octet| octet.len() == 2 && octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

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

/// Access control settings.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessSettings {
    /// Web admin credentials; absent until apid sets them.
    #[serde(rename = "webAdmin", default, skip_serializing_if = "Option::is_none")]
    pub web_admin: Option<WebAdminSettings>,
    /// How this device was claimed (schema v11); absent until it is, and
    /// absent on one claimed device by design — see [`ClaimSettings`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimSettings>,
    /// SSH channel policy.
    #[serde(default)]
    pub ssh: SshSettings,
    /// Local console policy.
    #[serde(default)]
    pub console: ConsoleSettings,
    /// Device credential metadata; never holds a plaintext secret.
    #[serde(default)]
    pub device: DeviceCredentialSettings,
    /// Bearer API tokens, hashes only.
    ///
    /// Empty by default, and empty is not written out: a device that never
    /// minted a token has a v8 document identical to its v7 form but for the
    /// version integer, which is what makes the v7 -> v8 bump additive and the
    /// A/B rollback survivable (see [`crate::MigrateV7ToV8`]).
    #[serde(rename = "apiTokens", default, skip_serializing_if = "Vec::is_empty")]
    pub api_tokens: Vec<ApiToken>,
}

/// Web admin credentials, written by apid.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebAdminSettings {
    /// Argon2id password hash in PHC string format.
    pub password_hash: String,
}

/// How the device left the unclaimed state (schema v11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSettings {
    /// Which channel minted the first administrator credential.
    pub via: ClaimChannel,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// claim committed, saturating at 0.
    pub at: u64,
    /// Whether the credential that claimed the device is still a bootstrap
    /// secret and must be rotated before the device accepts any other
    /// authenticated write.
    #[serde(rename = "rotationRequired")]
    pub rotation_required: bool,
}

/// Which channel claimed the device.
///
/// Exactly the two channels that can mint a first administrator credential.
/// There is no `unknown` member: a third channel would be an unauthenticated
/// write nobody decided to add, and naming one here would make room for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimChannel {
    /// `POST /api/v1/setup`.
    Setup,
    /// A provisioning document.
    ProvisioningDocument,
}

/// SSH channel policy, reconciled into dropbear's arguments and the managed
/// accounts' `~/.ssh/authorized_keys`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshSettings {
    /// Whether dropbear is started.
    pub enabled: bool,
    /// TCP port dropbear listens on.
    pub port: u16,
    /// Whether the root account may log in; phase 1 has only that account.
    #[serde(rename = "permitRootLogin")]
    pub permit_root_login: bool,
    /// Whether password authentication is offered; phase 1 auth is the device
    /// password.
    #[serde(rename = "passwordAuthentication")]
    pub password_authentication: bool,
    /// Addresses dropbear binds to (at most 10); empty means every address.
    #[serde(rename = "listenAddresses")]
    pub listen_addresses: Vec<String>,
    /// Public keys rendered into every managed account's `authorized_keys`
    /// file.
    #[serde(rename = "authorizedKeys")]
    pub authorized_keys: Vec<AuthorizedKey>,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 22,
            permit_root_login: true,
            password_authentication: true,
            listen_addresses: Vec::new(),
            authorized_keys: Vec::new(),
        }
    }
}

/// One SSH public key authorized to log in.
///
/// The comment lives in its own field rather than inside `key` so that the
/// canonical key text is what duplicate detection runs on: two operators
/// pasting the same key under different labels must not end up with two
/// entries granting the same access.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedKey {
    /// Canonical single-line key text, `<type> <base64blob>`, with no comment.
    pub key: String,
    /// Operator-supplied label; absent when the key was pasted without one.
    #[serde(rename = "comment", default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Local console policy.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConsoleSettings {
    /// Whether the tty3 root shell is started; only the `debug` image profile
    /// ships that shell at all.
    #[serde(rename = "shellEnabled")]
    pub shell_enabled: bool,
}

/// Device credential metadata.
///
/// Holds the hash of the per-device password and its revision, never the
/// password itself. Both stay `None`/`0` in a freshly built tree: a non-empty
/// default here would be a fleet-wide shared secret baked into the signed
/// rootfs.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceCredentialSettings {
    /// Argon2id password hash in PHC string format; absent until first boot
    /// generates the credential.
    #[serde(
        rename = "passwordHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub password_hash: Option<String>,
    /// Revision of the stored credential, bumped on every regeneration.
    pub generation: u32,
}

/// One bearer API token, as the settings tree holds it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiToken {
    /// Stable identity of this token, lowercase hex.
    ///
    /// Identity is this field and never a list position: an index is
    /// meaningful only against the list the caller last read, and a concurrent
    /// mint slides it onto a different entry.
    pub id: String,
    /// Operator-supplied label, the only thing that tells one token from
    /// another in a listing.
    pub name: String,
    /// SHA-256 hex digest of the token secret, lowercase, 64 characters.
    ///
    /// SHA-256 and not argon2id deliberately: the secret is machine-generated
    /// and has nothing to guess, so a work factor would buy no security and
    /// would be paid on every API request rather than once per login.
    pub hash: String,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// token was minted, saturating at 0.
    pub created: u64,
}

/// First-boot self-provisioning status.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvisioningSettings {
    /// Whether first-boot provisioning has run to completion.
    pub state: ProvisioningState,
    /// Device identity assigned at first boot, lowercase hex.
    #[serde(rename = "deviceId", default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Seeding revision that produced this tree.
    #[serde(rename = "seededGeneration")]
    pub seeded_generation: u32,
    /// The provisioning-document record (schema v10); absent until a document
    /// has been offered to this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ProvisioningDocumentSettings>,
}

/// What the last provisioning document did to this device.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvisioningDocumentSettings {
    /// `version` of the document last APPLIED, absent when none ever was.
    ///
    /// The DOCUMENT's own schema version, which moves independently of
    /// [`SCHEMA_VERSION`]: a document format revision does not reshape the
    /// settings tree and a settings bump does not invalidate a document.
    #[serde(
        rename = "appliedVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_version: Option<u32>,
    /// Digest of the document last applied, lowercase hex.
    ///
    /// The short-circuit that makes a re-apply a no-op: an offered document
    /// whose digest equals this one is not applied again. Over a CANONICAL
    /// rendering of the parsed document, so a comment, a reordered key or a
    /// changed indentation in the source file is the same document.
    #[serde(
        rename = "appliedDigest",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_digest: Option<String>,
    /// The last import ATTEMPT, applied or not.
    ///
    /// Distinct from the two fields above on purpose: a rejected document
    /// leaves them exactly as they were and lands only here, so a bad file on
    /// a stick can never make a device look configured by it.
    #[serde(
        rename = "lastImport",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_import: Option<ProvisioningImport>,
}

/// One provisioning-document import attempt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningImport {
    /// Which transport offered the document: `boot` or `media`.
    pub source: String,
    /// How it ended: `applied`, `unchanged` or `rejected`.
    pub outcome: String,
    /// Why it was rejected, naming the offending KEY PATH and never its value;
    /// absent for an outcome that is not a rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Seconds since the UNIX epoch as the device clock read them, saturating
    /// at 0.
    pub at: u64,
}

/// Stage of first-boot self-provisioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisioningState {
    /// The device has not provisioned itself yet.
    #[default]
    Pending,
    /// First-boot provisioning finished; the tree is the device's own.
    Complete,
}

/// A staged reset intent (schema v12).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetSettings {
    /// Which tier is staged. There is no parameterless reset.
    pub tier: ResetTier,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// intent committed, saturating at 0.
    pub requested: u64,
    /// The presence mechanism that authorized a presence-gated tier; absent
    /// for the tiers that are authenticated management actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<String>,
}

/// Which reset tier is staged — the tier table, whose
/// rows are the whole of the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResetTier {
    /// Tier 1: return the modelled settings to their schema defaults.
    Configuration,
    /// Tier 2: remove operator applications and their data.
    ApplicationData,
    /// Tier 3: return the device to its first-boot state, keeping identity,
    /// calibration, META and both system slots.
    FullFactory,
}

/// Characters a device identifier occupies: 16 bytes spelled in lowercase hex.
pub const DEVICE_ID_LEN: usize = 32;

/// The shortest administrator bootstrap password a provisioning document may
/// carry.
pub const MIN_ADMIN_PASSWORD_LEN: usize = 8;

/// Refuse a `provisioning.deviceId` that is not the identifier
/// `micad`'s `identity` module mints.
pub fn validate_device_id(device_id: &str) -> Result<(), String> {
    if device_id.len() != DEVICE_ID_LEN
        || !device_id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(format!(
            "a device identifier is exactly {DEVICE_ID_LEN} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

/// WiFi settings, reconciled by connd into wpa_supplicant and hostapd.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiSettings {
    /// Station (uplink) configuration.
    pub client: WifiClientSettings,
    /// Access-point (provisioning) configuration.
    pub ap: WifiApSettings,
}

/// WiFi station configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiClientSettings {
    /// Whether the station role is started.
    pub enabled: bool,
    /// Interface the station role runs on.
    pub interface: String,
    /// Known networks, most preferred by `priority`.
    ///
    /// Written as a whole JSON array through the dot-path API; the path syntax
    /// has no array indexing.
    pub networks: Vec<WifiNetwork>,
}

impl Default for WifiClientSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: "wlan0".to_string(),
            networks: Vec::new(),
        }
    }
}

/// One known WiFi network.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiNetwork {
    /// Network name.
    pub ssid: String,
    /// Pre-shared key; absent means an open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    /// Whether the network hides its SSID.
    #[serde(default)]
    pub hidden: bool,
    /// Selection preference; higher wins.
    #[serde(default)]
    pub priority: i32,
}

/// Characters a raw 256-bit pre-shared key occupies, spelled in hex.
pub const RAW_PMK_LEN: usize = 64;

/// IEEE 802.11i's shortest WPA2 passphrase.
pub const MIN_PASSPHRASE_LEN: usize = 8;

/// IEEE 802.11i's longest.
pub const MAX_PASSPHRASE_LEN: usize = 63;

/// True when `value` can be carried inside a wpa_supplicant double-quoted
/// string with no way of ending the string early.
#[must_use]
pub fn is_wpa_quotable(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'"' && byte != b'\\')
}

/// Refuse a [`WifiNetwork::psk`] no WPA2 supplicant could use.
///
/// The bound is IEEE 802.11i's and it is checked for the reason the access
/// point checks it: wpa_supplicant rejects an out-of-range passphrase by
/// refusing the WHOLE configuration file, which takes every other configured
/// network down with it while the reconcile still reports `applied`.
pub fn validate_wifi_psk(psk: &str) -> Result<(), String> {
    if psk.len() == RAW_PMK_LEN && psk.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    if psk.len() < MIN_PASSPHRASE_LEN || psk.len() > MAX_PASSPHRASE_LEN {
        return Err(format!(
            "a WPA2 passphrase is {MIN_PASSPHRASE_LEN} to {MAX_PASSPHRASE_LEN} characters \
             (or a {RAW_PMK_LEN}-digit hex PMK)"
        ));
    }
    // A passphrase has no hex form -- bare hex means a raw PMK, not a
    // passphrase -- so the renderer has nothing to fall back to and refuses.
    // Refusing here instead means a key the renderer cannot carry is never
    // accepted, rather than stored and dead. The sentence names neither the
    // value nor its length, for the reason the length bound's does not.
    if !is_wpa_quotable(psk) {
        return Err(
            "the pre-shared key contains a character wpa_supplicant configuration \
             cannot carry; use printable ASCII without a quote or a backslash"
                .to_string(),
        );
    }
    Ok(())
}

/// WiFi access-point configuration used by the provisioning flow.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WifiApSettings {
    /// When the access point runs.
    pub mode: ApMode,
    /// Interface the access point runs on.
    pub interface: String,
    /// Advertised SSID; absent means derive it from the device identity at
    /// render time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
    /// Pre-shared key; absent means derive it from the device credential at
    /// render time. A fleet-wide constant default is forbidden
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    /// 2.4 GHz channel the access point uses.
    pub channel: u8,
    /// Regulatory domain the radio is configured for.
    #[serde(rename = "countryCode")]
    pub country_code: String,
    /// AP-side address in CIDR notation.
    pub address: String,
    /// Seconds without a usable uplink before the access point starts.
    #[serde(rename = "holdDownSeconds")]
    pub hold_down_seconds: u32,
    /// Seconds the access point stays up after an uplink is restored.
    #[serde(rename = "graceSeconds")]
    pub grace_seconds: u32,
}

impl Default for WifiApSettings {
    fn default() -> Self {
        Self {
            mode: ApMode::Off,
            interface: "wlan0".to_string(),
            ssid: None,
            psk: None,
            channel: 6,
            country_code: "US".to_string(),
            address: "192.168.4.1/24".to_string(),
            hold_down_seconds: 120,
            grace_seconds: 60,
        }
    }
}

/// When the WiFi access point runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApMode {
    /// Never.
    #[default]
    Off,
    /// Only while no usable uplink exists.
    Provisioning,
    /// Always, regardless of the uplink.
    Always,
}

/// What kind of link a `network` entry describes.
///
/// Absent means [`IfaceKind::Physical`], and a physical entry never serializes
/// the field: a v6 tree of physical interfaces and its v7 form differ by the
/// schema version integer alone, which is what makes the v6 -> v7 bump
/// additive and the A/B rollback survivable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IfaceKind {
    /// A NIC the kernel already has.
    #[default]
    Physical,
    /// An 802.1Q VLAN on top of another declared entry.
    Vlan,
    /// A software bridge over other declared entries.
    Bridge,
    /// A WireGuard tunnel.
    Wireguard,
}

impl IfaceKind {
    /// Whether this is the default kind, the one that is never written out.
    fn is_physical(&self) -> bool {
        matches!(self, Self::Physical)
    }
}

/// Network configuration for a single interface.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IfaceSettings {
    /// What kind of link this is; absent means physical.
    #[serde(default, skip_serializing_if = "IfaceKind::is_physical")]
    pub kind: IfaceKind,
    /// Whether the interface acquires its address via DHCP.
    pub dhcp: bool,
    /// Static addressing, used when `dhcp` is false.
    #[serde(rename = "static", default, skip_serializing_if = "Option::is_none")]
    pub static_: Option<StaticConfig>,
    /// VLAN parameters, for `kind = "vlan"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<VlanConfig>,
    /// Bridge parameters, for `kind = "bridge"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeConfig>,
    /// WireGuard parameters, for `kind = "wireguard"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardConfig>,
    /// Static routes this interface carries, beyond the default route a
    /// `static.gateway` declares.
    ///
    /// Empty and skipped when empty, so an entry that declares none
    /// serializes exactly as it did before routes existed: a document written
    /// by a build that has this field is byte-identical to one written by a
    /// build that does not, until someone uses it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteConfig>,
    /// The DHCP server this interface offers, if it offers one.
    #[serde(
        rename = "dhcpServer",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub dhcp_server: Option<DhcpServerConfig>,
}

/// One static route, rendered as networkd's `[Route]`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    /// Where the route leads, in CIDR notation. `0.0.0.0/0` is the default
    /// route, which is what `static.gateway` already writes -- declaring both
    /// is refused rather than rendered twice.
    pub destination: String,
    /// The next hop. Absent is a route out of this interface with no gateway,
    /// which is what an on-link route is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Route metric; absent leaves networkd's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u32>,
}

/// The DHCP server offered on an interface, rendered as networkd's
/// `[DHCPServer]`.
///
/// The pool is expressed the way networkd expresses it -- an offset into the
/// interface's own subnet and a count -- rather than as a first and last
/// address, because that is what the rendered file takes and a range converted
/// twice is a range that can disagree with itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DhcpServerConfig {
    /// First address handed out, as an offset from the subnet address.
    #[serde(rename = "poolOffset")]
    pub pool_offset: u32,
    /// How many addresses the pool holds.
    #[serde(rename = "poolSize")]
    pub pool_size: u32,
    /// DNS servers announced to clients; empty announces none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<String>,
    /// Default lease time in seconds; absent leaves networkd's own default.
    #[serde(
        rename = "leaseSeconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub lease_seconds: Option<u32>,
}

/// The 802.1Q parameters of a VLAN interface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VlanConfig {
    /// Name of the `network` entry this VLAN sits on.
    pub parent: String,
    /// 802.1Q VLAN id.
    pub id: u16,
}

/// The parameters of a software bridge.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Names of the `network` entries enslaved to this bridge.
    #[serde(default)]
    pub ports: Vec<String>,
}

/// The parameters of a WireGuard tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireguardConfig {
    /// UDP port to listen on. Absent lets the kernel pick one, which is what a
    /// client that only ever initiates wants.
    #[serde(
        rename = "listenPort",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub listen_port: Option<u16>,
    /// The far ends of the tunnel.
    #[serde(default)]
    pub peers: Vec<WireguardPeer>,
}

/// One far end of a WireGuard tunnel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireguardPeer {
    /// The peer's base64 X25519 public key.
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// CIDRs routed to this peer.
    #[serde(rename = "allowedIps", default)]
    pub allowed_ips: Vec<String>,
    /// `host:port` to send to, for a peer this end initiates to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Keepalive interval in seconds, for a peer behind NAT.
    #[serde(
        rename = "persistentKeepalive",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub persistent_keepalive: Option<u16>,
}

/// Linux `IFNAMSIZ` minus the terminator: the longest name an interface can
/// actually have.
const MAX_IFACE_NAME_LEN: usize = 15;

/// Refuse a `network` map key the kernel could not name an interface.
fn validate_network_key(iface: &str) -> Result<(), String> {
    if iface.is_empty() {
        return Err("network interface name is empty".to_string());
    }
    if iface.len() > MAX_IFACE_NAME_LEN {
        return Err(format!(
            "network interface {iface:?} is longer than {MAX_IFACE_NAME_LEN} characters"
        ));
    }
    if iface == "." || iface == ".." {
        return Err(format!("network interface {iface:?} is not a name"));
    }
    if !iface
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    {
        return Err(format!(
            "network interface {iface:?} contains a character an interface name cannot have"
        ));
    }
    Ok(())
}

/// Static addressing for a single interface.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticConfig {
    /// Interface address in CIDR notation, e.g. `"192.168.1.10/24"`.
    pub address: String,
    /// Default gateway address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// DNS server addresses.
    #[serde(default)]
    pub dns: Vec<String>,
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
            validate_network_key(iface).map_err(|message| SettingsError::Validation {
                path: path.to_string(),
                message,
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
        *self = candidate;
        Ok(())
    }

    fn to_json(&self) -> Result<Value, SettingsError> {
        serde_json::to_value(self).map_err(|err| SettingsError::Parse(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrips_via_toml() {
        let settings = Settings::default();
        let text = toml::to_string(&settings).unwrap();
        let parsed: Settings = toml::from_str(&text).unwrap();
        assert_eq!(parsed, settings);
        assert_eq!(parsed.hostname, "mica");
        assert!(parsed.network.is_empty());
        assert!(parsed.access.web_admin.is_none());
        assert_eq!(parsed.access.ssh, SshSettings::default());
        assert_eq!(parsed.access.console, ConsoleSettings::default());
        assert_eq!(parsed.access.device, DeviceCredentialSettings::default());
        assert!(parsed.access.api_tokens.is_empty());
        assert_eq!(parsed.provisioning, ProvisioningSettings::default());
        assert_eq!(parsed.wifi, WifiSettings::default());
        assert_eq!(parsed.container, ContainerSettings::default());
        assert_eq!(parsed.mqtt, MqttSettings::default());
        assert_eq!(parsed.time, TimeSettings::default());
    }

    /// The time defaults, spelled out: no managed servers (the image fallback
    /// pool applies) and the UTC presentation zone the contract starts from.
    #[test]
    fn time_defaults_are_no_servers_and_utc() {
        let settings = Settings::default();
        assert!(settings.time.ntp.servers.is_empty());
        assert_eq!(settings.time.timezone, "UTC");

        // There is deliberately no switch to find here: timesyncd is an
        // always-running base service, and a field named like one appearing
        // in this subtree is the regression this pins against.
        let time = toml::to_string(&settings.time).unwrap();
        assert!(!time.contains("enabled"), "{time}");
        assert!(!time.contains("Poll"), "{time}");
    }

    /// The renderer writes `NTP=` space-separated into an ini drop-in, so
    /// everything that could end the assignment or smuggle another is refused
    /// at the write surface rather than stored and dead at render time.
    #[test]
    fn an_ntp_server_the_renderer_cannot_carry_is_refused() {
        for server in [
            "",
            "pool one.example",
            "pool\tone",
            "two\nlines",
            "a=b",
            "#comment",
            "host_name.example",
            "höst.example",
        ] {
            assert!(
                validate_ntp_servers(&[server.to_string()]).is_err(),
                "{server:?} must be refused"
            );
        }

        assert!(
            validate_ntp_servers(&[
                "0.debian.pool.ntp.org".to_string(),
                "time.example-corp.com".to_string(),
                "192.0.2.7".to_string(),
                "2001:db8::123".to_string(),
            ])
            .is_ok()
        );
    }

    #[test]
    fn the_ntp_server_list_is_bounded_and_duplicate_free() {
        let too_many: Vec<String> = (0..=MAX_NTP_SERVERS)
            .map(|index| format!("ntp{index}.example"))
            .collect();
        let err = validate_ntp_servers(&too_many).unwrap_err();
        assert!(err.contains(&MAX_NTP_SERVERS.to_string()), "{err}");

        let twice = vec!["ntp.example".to_string(), "ntp.example".to_string()];
        let err = validate_ntp_servers(&twice).unwrap_err();
        assert!(err.contains("twice"), "{err}");

        let long = "a".repeat(254);
        assert!(validate_ntp_servers(&[long]).is_err());
    }

    /// Deterministic on every host: the rule is the tzdata name grammar and
    /// never a lookup against the machine's own zoneinfo tree.
    #[test]
    fn a_timezone_is_validated_by_grammar_not_by_the_host_tzdata() {
        for zone in [
            "UTC",
            "Etc/GMT+8",
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "America/Port-au-Prince",
            // Grammatically fine and almost certainly not a real zone: the
            // existence check belongs to reconcile time, not to this rule.
            "Atlantis/Made_Up",
        ] {
            assert!(validate_timezone_name(zone).is_ok(), "{zone:?}");
        }
        for zone in [
            "",
            "/Etc/UTC",
            "Etc/",
            "Etc//UTC",
            "../etc/shadow",
            "Europe/..",
            "Europe/Ber lin",
            "Europe/Berlin\n",
            "Europe/Bërlin",
            &"Z/".repeat(40),
        ] {
            assert!(validate_timezone_name(zone).is_err(), "{zone:?}");
        }
    }

    /// The write surface enforces the two `time` predicates through the tree
    /// itself, so no caller of `Settings::set` can store what the reconciler
    /// cannot render — and an unrelated write leaves a hand-edited `time`
    /// subtree alone, the same property the network keys have.
    #[test]
    fn a_time_write_is_validated_and_an_unrelated_write_is_not() {
        let mut settings = Settings::default();
        settings
            .set("time.timezone", Value::from("Europe/Berlin"))
            .unwrap();
        assert_eq!(settings.time.timezone, "Europe/Berlin");

        let err = settings
            .set("time.timezone", Value::from("Europe/Ber lin"))
            .unwrap_err();
        assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
        assert_eq!(settings.time.timezone, "Europe/Berlin");

        settings
            .set(
                "time.ntp.servers",
                serde_json::json!(["0.pool.ntp.org", "192.0.2.7"]),
            )
            .unwrap();
        assert_eq!(settings.time.ntp.servers.len(), 2);
        let err = settings
            .set("time.ntp.servers", serde_json::json!(["bad server"]))
            .unwrap_err();
        assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
        assert_eq!(settings.time.ntp.servers.len(), 2);

        // An unrelated write over a tree whose `time` subtree would no longer
        // validate must still land: the rule is about the write, not the tree.
        let mut hand_edited: Settings = settings.clone();
        hand_edited.time.timezone = "not a zone!".to_string();
        hand_edited.set("hostname", Value::from("edge-42")).unwrap();
        assert_eq!(hand_edited.hostname, "edge-42");
        assert_eq!(hand_edited.time.timezone, "not a zone!");
    }

    /// The MQTT defaults, spelled out: off, loopback, and no auth. The switch
    /// is what an operator turns on; the listener is what the broker is
    /// rendered from, and neither constrains the other.
    #[test]
    fn mqtt_defaults_are_off_and_loopback() {
        let settings = Settings::default();
        assert!(!settings.mqtt.enabled);
        assert_eq!(settings.mqtt.listen.address, "127.0.0.1");
        assert_eq!(settings.mqtt.listen.port, 1883);
        assert!(!settings.mqtt.auth.enabled);

        // No credential field exists in this management subtree; the broker's
        // accounts live in a mode-restricted STATE file instead.
        let mqtt = toml::to_string(&settings.mqtt).unwrap();
        assert!(!mqtt.contains("password"), "{mqtt}");
        assert!(!mqtt.contains("username"), "{mqtt}");
    }

    #[test]
    fn no_secret_is_present_in_a_freshly_built_tree() {
        // A non-None default here would be a fleet-wide shared secret baked
        // into a byte-identical signed rootfs.
        let settings = Settings::default();
        assert_eq!(settings.access.device.password_hash, None);
        assert_eq!(settings.access.device.generation, 0);
        assert_eq!(settings.access.web_admin, None);
        assert!(settings.access.api_tokens.is_empty());
        assert_eq!(settings.wifi.ap.psk, None);
        assert_eq!(settings.wifi.ap.ssid, None);
        assert!(settings.wifi.client.networks.is_empty());
        assert_eq!(settings.provisioning.device_id, None);

        let text = toml::to_string(&settings).unwrap();
        assert!(
            !text.contains("psk"),
            "serialized tree must hold no key: {text}"
        );
        assert!(
            !text.contains("passwordHash"),
            "serialized tree must hold no credential: {text}"
        );
        assert!(
            !text.contains("apiTokens"),
            "serialized tree must hold no credential: {text}"
        );
    }

    /// The empty list is not written out, and that is what makes the v7 -> v8
    /// bump additive: a device that never minted a token has a v8 document
    /// whose only difference from its v7 form is the version integer.
    #[test]
    fn an_empty_token_list_is_not_serialized() {
        let text = toml::to_string(&Settings::default()).unwrap();
        assert!(!text.contains("apiTokens"), "{text}");

        let mut with_token = Settings::default();
        with_token.access.api_tokens.push(sample_token());
        let text = toml::to_string(&with_token).unwrap();
        assert!(text.contains("[[access.apiTokens]]"), "{text}");
    }

    /// The wire names are the tree's camelCase convention, and the entry
    /// carries the four fields and no fifth.
    #[test]
    fn a_token_round_trips_through_toml_under_its_camel_case_name() {
        let mut settings = Settings::default();
        settings.access.api_tokens.push(sample_token());

        let text = toml::to_string(&settings).unwrap();
        let parsed: Settings = toml::from_str(&text).unwrap();
        assert_eq!(parsed, settings);

        // Through the dot-path API the JSON shape is the same one apid reads
        // out of `GetSettings("access")`.
        let value = settings.get("access.apiTokens").unwrap();
        let entry = &value.as_array().unwrap()[0];
        let fields: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(fields, ["created", "hash", "id", "name"]);
        assert_eq!(entry["id"], Value::String("3f2a9c41".to_string()));
        assert_eq!(entry["created"], Value::from(1_700_000_000_u64));
    }

    /// The path syntax has no array indexing, so mint and revoke are
    /// read-modify-write of the whole list. This pins that the whole-array
    /// write works and that the indexed one does not silently appear to.
    #[test]
    fn the_token_list_is_written_whole_and_not_by_index() {
        let mut settings = Settings::default();
        let one = serde_json::to_value([sample_token()]).unwrap();
        settings.set("access.apiTokens", one).unwrap();
        assert_eq!(settings.access.api_tokens.len(), 1);

        // An index is not a path segment; a write through one must not land.
        assert!(
            settings
                .set("access.apiTokens.0.name", Value::from("x"))
                .is_err()
        );
        assert_eq!(settings.access.api_tokens[0].name, "ci-deploy");

        settings
            .set("access.apiTokens", Value::Array(Vec::new()))
            .unwrap();
        assert!(settings.access.api_tokens.is_empty());
    }

    /// A passphrase the station renderer cannot carry is refused here, so it
    /// cannot be accepted at a write surface and then die at render time.
    #[test]
    fn a_passphrase_the_renderer_cannot_quote_is_refused() {
        for psk in [
            "has\"quote1",
            "has\\backslash",
            "two\nlines1",
            "tab\there1",
            "cafe\u{301}-latte",
            "caf\u{e9}-latte",
        ] {
            let Err(err) = validate_wifi_psk(psk) else {
                panic!("{psk:?} must be refused");
            };
            assert!(err.contains("pre-shared key"), "{psk:?}: {err}");
            // A refusal never echoes the value; a key is a secret and this
            // sentence reaches an HTTP client.
            assert!(!err.contains(psk), "the refusal echoed the key: {err}");
        }

        // Everything IEEE 802.11i's own passphrase alphabet allows and the
        // renderer can quote still passes, and so does a raw PMK.
        assert!(validate_wifi_psk("hunter2hunter2").is_ok());
        assert!(validate_wifi_psk("p@ssw0rd!#$%^&*()_+-=[]{};:',.<>/? ~`").is_ok());
        assert!(validate_wifi_psk(&"a".repeat(RAW_PMK_LEN)).is_ok());
    }

    /// One well-formed entry, spelled the way the store spells it.
    fn sample_token() -> ApiToken {
        ApiToken {
            id: "3f2a9c41".to_string(),
            name: "ci-deploy".to_string(),
            hash: "9".repeat(64),
            created: 1_700_000_000,
        }
    }
}
