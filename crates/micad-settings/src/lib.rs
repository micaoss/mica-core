//! Settings library for micad.

#![forbid(unsafe_code)]

mod absent;
mod api_token;
mod audit;
mod authorized_key;
// A public module rather than a re-export: `/mica/config/` is a namespace with
// several documents and two processes reading it, so callers name the
// namespace. It carries the update policy document and the
// baked layer it overrides.
pub mod configuration;
// The settings store's own occupants of that namespace, plus the STATE
// remainder. Private, because the store is the only thing that reads
// or writes them: `configuration`'s documents have two processes and these
// have one.
mod documents;
mod error;
mod features;
mod model;
mod network_rules;
mod network_write;
mod path;
mod recovery;
mod secret;
mod store;
mod transaction;

pub use absent::absent;
pub use api_token::{MAX_TOKENS, is_api_token_id, validate_api_tokens};
pub use audit::{
    ACTOR_DEVICE, ACTOR_OPERATOR, ACTOR_POLICY, AUDIT_LOG, AUDIT_LOG_PREVIOUS, AUDIT_ROTATE_BYTES,
    DEFAULT_AUDIT_RING_DIR, REQUESTED, UPDATE_CHECK_EVENT, UPDATE_CONFIG_EVENT, UPDATE_FETCH_EVENT,
    UPDATE_INSTALL_EVENT, append_audit_line, audit_line, audit_ring_dir,
};
pub use authorized_key::{
    MAX_KEYS, decode_base64, encode_base64_nopad, parse_authorized_key, ssh_fingerprint,
    validate_authorized_keys,
};
pub use documents::{
    BLUETOOTH_DOCUMENT, BLUETOOTH_SCHEMA_VERSION, BluetoothDocument, CONFIG_DOCUMENTS,
    CONTAINER_DOCUMENT, CONTAINER_SCHEMA_VERSION, ContainerDocument, DEFAULT_CONFIG_DIR,
    DNS_DOCUMENT, DNS_SCHEMA_VERSION, DOCUMENT_MODE, DOCUMENT_SUBTREES, DnsDocument, MQTT_DOCUMENT,
    MQTT_SCHEMA_VERSION, MqttDocument, NETWORK_DOCUMENT, NETWORK_SCHEMA_VERSION, NetworkDocument,
    SERVICES_DOCUMENT, SERVICES_SCHEMA_VERSION, SSH_DOCUMENT, SSH_SCHEMA_VERSION,
    STATE_SCHEMA_VERSION, SYSTEM_DOCUMENT, SYSTEM_SCHEMA_VERSION, ServicesDocument, SshDocument,
    StateAccessSettings, StateDocument, SystemDocument, TIME_DOCUMENT, TIME_SCHEMA_VERSION,
    TimeDocument, WIFI_DOCUMENT, WIFI_SCHEMA_VERSION, WifiDocument, document_subtrees,
};
pub use error::SettingsError;
pub use features::{
    ClaimDeviceCode, ContainerEngine, Feature, Features, Init, PRODUCT_FILE, product_name,
    product_ssh, product_switch,
};
pub use model::{
    APID_LISTENERS_PATH, APID_LOCAL_SOCKET_PATH, AccessSettings, ApMode, ApPool, ApSecurity,
    ApiToken, AuthorizedKey, BleProvisioningSettings, BluetoothSettings, BridgeConfig, CanConfig,
    ClaimChannel, ClaimSettings, ConsoleSettings, ContainerSettings, DEVICE_ID_LEN,
    DeviceCredentialSettings, DnsSettings, GlobalDns, IfaceKind, IfaceSettings,
    MAX_CONTAINER_PROJECT_BYTES, MAX_CONTAINER_PROJECTS, MAX_CONTAINER_PROJECTS_BYTES,
    MAX_GLOBAL_DNS_SERVERS, MAX_NTP_SERVERS, MAX_PASSPHRASE_LEN, MAX_RECORD_COMMENT_CHARS,
    MAX_RECORD_ID_LEN, MAX_STATIC_RECORDS, MAX_USER_UNIT_BYTES, MAX_USER_UNIT_CONF_BYTES,
    MAX_USER_UNITS, MAX_USER_UNITS_BYTES, MIN_ADMIN_PASSWORD_LEN, MIN_LEASE_SECONDS,
    MIN_PASSPHRASE_LEN, MqttAuthSettings, MqttListenSettings, MqttSettings, NtpSettings,
    PairedDevice, ProvisioningDocumentSettings, ProvisioningImport, ProvisioningSettings,
    ProvisioningState, RAW_PMK_LEN, ResetSettings, ResetTier, RouteConfig, ServicesSettings,
    Settings, SshSettings, StaticConfig, StaticRecord, TimeSettings, UserUnit, VlanConfig,
    WebAdminSettings, WebMode, WebSettings, WifiApSettings, WifiClientSettings, WifiNetwork,
    WifiSettings, WireguardConfig, WireguardPeer, ap_channels, is_bluetooth_address, is_host_name,
    is_openrc_script_name, is_project_name, is_systemd_template, is_systemd_unit_name,
    is_wpa_quotable, unit_install_values, validate_ap_pool, validate_bluetooth,
    validate_container_project_dirs, validate_container_projects, validate_device_id, validate_dns,
    validate_ntp_servers, validate_services, validate_timezone_name, validate_wifi_psk,
};
pub use network_rules::{
    MAX_CAN_BITRATE, MAX_CAN_DATA_BITRATE, MAX_CAN_RESTART_MS, MAX_IFACE_LEN, NetworkViolation,
    check_iface_name, is_host_port, is_ip_or_cidr, is_wireguard_key, topology_violations,
    validate_can, validate_peers, validate_topology,
};
pub use network_write::{
    entry_violations, is_default_destination, map_violations, references_to, relational_violations,
    routing_problem, valid_cidr, valid_ipv4, validate_static_address, write_violations,
};
pub use path::{json_path_get, path_segments, quote_path_segment};
pub use recovery::{
    ACTIONS_KEY, CMDLINE_PATH_ENV, DECLARATION_PATH_ENV, DEFAULT_CMDLINE_PATH,
    DEFAULT_DECLARATION_PATH, DEFAULT_PRESENCE_MARKER_PATH, Declaration, INTENT_PARAMETER,
    INTENT_SOURCE, NoAction, PRESENCE_MARKER_PATH_ENV, PRESENCE_WINDOW_SECS, PresenceMarker,
    RECOVERY_ACTION_EVENT, REFUSED_MALFORMED_INTENT, RecoveryAction, TIER_NONE, cmdline_path,
    credential_recovery_event, declaration_path, intent_from_cmdline, presence_marker_path,
    recovery_action_event, refusal_outcome, reset_event,
};
pub use secret::{hash_password, random_bytes, verify_password};
pub use store::{DEFAULT_PATH, DocumentRefusal, LoadedStore, Store};
