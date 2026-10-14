//! The routes' paths and the fixed strings the handlers share.

/// The reserved prefix, and the paths declared under it.
///
/// The leaves are the paths as the nested router sees them; the OpenAPI
/// document composes them with the prefix through `context_path`, and
/// [`is_declared_api_route`] composes them to get what the gate sees. One
/// spelling each.
pub(crate) const API: &str = "/api";
pub(super) const VERSIONS_PATH: &str = "/versions";
pub(super) const V1_META_PATH: &str = "/v1/meta";

/// The second, differently-scoped health endpoint.
///
/// Not `/healthz` and never a replacement for it: `/healthz` answers *"is
/// apid's listener up"* and this answers *"is this appliance manageable"*.
/// Both sentences are true and neither implies the other, which is why there
/// are two paths and not one.
pub(super) const V1_HEALTH_PATH: &str = "/v1/health";

/// The live-state key the health route probes, and the value it reports as
/// `checkedAt`.
///
/// One bus call answers both questions a health check asks. It proves the round
/// trip — micad serves this key by reading `/proc/uptime` at request time,
/// so a value coming back means a real
/// exchange happened and not that a cached flag was read — and the value it
/// returns is the only clock on this appliance a health answer may be stamped
/// with, there being no trusted wall clock anywhere in the crate.
pub(super) const HEALTH_PROBE_PATH: &str = "uptime";

/// The actions namespace, with its one shipped verb. A password change is
/// an operation and not a resource — the namespace is named `actions`
/// precisely so no reader expects a `GET` to work there.
pub(super) const V1_CHANGE_PASSWORD_PATH: &str = "/v1/actions/change-password";

/// The two read-only roots, in the three spellings they need.
///
/// The prefix is the shared one and the only one the gate predicate tests. The
/// other two exist because axum names a wildcard segment `{*path}` and OpenAPI
/// names a template parameter `{path}`, so the served path and the documented
/// path cannot be the same string; `the_resource_path_spellings_agree` holds
/// them to the prefix so they cannot drift apart.
#[cfg(test)]
pub(super) const V1_SETTINGS_PREFIX: &str = "/v1/settings/";
pub(super) const V1_SETTINGS_ROUTE: &str = "/v1/settings/{*path}";
pub(super) const V1_SETTINGS_DOC: &str = "/v1/settings/{path}";
#[cfg(test)]
pub(super) const V1_STATE_PREFIX: &str = "/v1/state/";
pub(super) const V1_STATE_ROUTE: &str = "/v1/state/{*path}";
pub(super) const V1_STATE_DOC: &str = "/v1/state/{path}";

/// The action route for a WireGuard key rotation. A new route, and
/// therefore additive.
///
/// One spelling and not three, unlike the resource roots above: `{iface}` is a
/// single-segment parameter, which axum and OpenAPI spell the same way, so
/// there is nothing here for a test to hold together. The prefix and the leaf
/// exist separately because [`is_declared_api_route`] has to recognise the
/// shape without a router to ask.
/// The token collection and its item route.
///
/// The item route needs its prefix separately for the same reason the rotate
/// action does: [`is_declared_api_route`] has to recognise the shape with no
/// router to ask.
pub(super) const V1_TOKENS_PATH: &str = "/v1/tokens";
pub(super) const V1_TOKEN_ROUTE: &str = "/v1/tokens/{id}";

/// The dot-path the token collection lives at, which every envelope raised
/// about it names.
pub(super) const API_TOKENS_PATH: &str = "access.apiTokens";

pub(super) const V1_WIREGUARD_ROTATE_ROUTE: &str = "/v1/actions/wireguard/{iface}/rotate-key";

/// The three action verbs.
///
/// No collection, no identifier and nothing to read back, so each is one
/// constant rather than the prefix/route/doc triple a resource path needs.
pub(super) const V1_REBOOT_PATH: &str = "/v1/actions/reboot";
pub(super) const V1_POWEROFF_PATH: &str = "/v1/actions/poweroff";
pub(super) const V1_TRANSIENT_PASSWORD_PATH: &str = "/v1/actions/transient-root-password";

/// Apply-task history and one task by id.
pub(super) const V1_TASKS_PATH: &str = "/v1/tasks";
pub(super) const V1_TASK_ROUTE: &str = "/v1/tasks/{id}";

/// The read-only time-synchronization status.
///
/// A fixed path like the action verbs, not a resource triple: the status is
/// observed from timesyncd at request time, names no dot-path, and has no
/// write counterpart — there is deliberately no route beside it that could
/// pause or stop synchronization.
pub(super) const V1_TIME_STATUS_PATH: &str = "/v1/time/status";

/// The read-only storage status.
///
/// A fixed path on the time status's reasoning, and one more besides: this is
/// the whole of the storage surface. There is no sibling constant here for a
/// format, repartition, resize, wipe or mount action, and
/// [`crate::tests`]'s route-surface scan asserts that there is not — the
/// layout is fixed by the image assembler, and an API that could rewrite it
/// would be a remote destructive surface with no product use.
pub(super) const V1_STORAGE_STATUS_PATH: &str = "/v1/storage/status";

/// The system-information surface: what this device is, in one
/// read. A fixed path on the time status's reasoning: observed at request
/// time, no dot-path, no write counterpart.
pub(super) const V1_SYSTEM_INFO_PATH: &str = "/v1/system/info";

/// Board telemetry: temperature, watchdog, reset reason. Same
/// shape and same reasoning.
pub(super) const V1_SYSTEM_TELEMETRY_PATH: &str = "/v1/system/telemetry";

/// One service's log, by the allowlist micad holds.
pub(super) const V1_SYSTEM_LOG_ROUTE: &str = "/v1/system/logs/{source}";

/// The OBSERVED network state, distinct from `/v1/network`'s
/// desired map: carrier, addresses, lease, routes, DNS, association and the
/// radio capabilities, and nothing from the settings tree.
///
/// A static segment under the prefix `/v1/network/{iface}` is declared on:
/// the router matches the static route first, so an interface literally
/// named `status` is not addressable through the item route. That is
/// the cost of spelling
/// the three status surfaces the same way.
pub(super) const V1_NETWORK_STATUS_PATH: &str = "/v1/network/status";

/// The diagnostic snapshot collection and its item route.
///
/// The collection takes `GET` (list) and `POST` (collect one, bounded); the
/// item takes `GET` (export) and `DELETE` (the explicit retention
/// operation). There is no route that uploads a snapshot anywhere, and none
/// that reads a source the snapshot schema does not name.
pub(super) const V1_DIAGNOSTICS_SNAPSHOTS_PATH: &str = "/v1/diagnostics/snapshots";
pub(super) const V1_DIAGNOSTICS_SNAPSHOT_ROUTE: &str = "/v1/diagnostics/snapshots/{id}";

/// The first-run route.
///
/// Not under `/v1/actions/`: it is neither a settings
/// write (it writes three subtrees), nor a collection, and calling it an
/// action understates that it is the device's one unauthenticated write. It is
/// the first-run operation, so it is named for that and nothing else.
pub(super) const V1_SETUP_PATH: &str = "/v1/setup";

/// Where an uploaded update archive lands, on a device.
///
/// `uploads/` inside the acquisition workspace, which is the directory micad's
/// `ImportUpdate` bounds its argument to. On the same medium as the objects
/// the archive carries, so a device with no room for the deployment runs out
/// of it while writing the upload -- the earlier and cheaper failure.
pub(super) const UPDATE_UPLOADS_DIR: &str = "/mica/updates/uploads";

/// The read-only provisioning-document status.
///
/// A fixed path on the time status's reasoning, and the whole of this
/// surface: there is no sibling constant here that applies, re-applies,
/// returns or clears a provisioning document. The document is the channel for
/// a device with NO network — it is applied by micad from a boot medium or a
/// stick before anything is listening — so an HTTP route that applied one
/// would be a second, differently-trusted write path for the same thing. The
/// handler is [`crate::provisioning_api::api_v1_provisioning_status`].
pub(crate) const V1_PROVISIONING_STATUS_PATH: &str = "/v1/provisioning/status";

/// The reset tiers, one path for all three.
///
/// A fixed path and a `POST` alone. The tier is in the BODY and not in the
/// path because naming the tier is part of
/// the request that gets audited, and a path segment per tier would make
/// "which resets does this device offer" a question about the route table
/// rather than about `ResetTier` — which is where the answer that there is no
/// fourth tier has to live. Not under `/v1/actions/` for the reason
/// [`V1_SETUP_PATH`] is not: this writes a device-lifecycle intent, and
/// calling it an action understates it.
pub(super) const V1_RESET_PATH: &str = "/v1/reset";

/// The dot-path the staged intent lives at, which every envelope about it
/// names.
pub(super) const RESET_PATH: &str = "reset";

/// Credential recovery, the one operation on
/// this surface whose authority is physical presence rather than a credential.
///
/// Under `recovery/` and not `actions/` deliberately: `actions` is the
/// namespace of things an authenticated operator does, and an authenticated
/// session may not run this one.
pub(super) const V1_RECOVERY_CREDENTIAL_PATH: &str = "/v1/recovery/credential";

/// Browser authentication state. Every operation stays inside the reserved
/// JSON API surface.
pub(super) const V1_SESSION_PATH: &str = "/v1/session";

/// How this device was claimed, and whether its credential must be rotated.
///
/// A fixed path and a `GET` alone: a claim is caused by
/// [`V1_SETUP_PATH`] or by a provisioning document, never by a write here.
/// Beside `/v1/session` rather than under it because it describes the DEVICE
/// and not the browser: the answer is the same for a bearer client.
pub(super) const V1_CLAIM_PATH: &str = "/v1/claim";
pub(super) const V1_UI_PATH: &str = "/v1/ui";
pub(super) const V1_UI_ACTIVE_PATH: &str = "/v1/ui/active";
pub(super) const V1_UI_BUNDLES_PATH: &str = "/v1/ui/bundles";
pub(super) const V1_UI_BUNDLE_ROUTE: &str = "/v1/ui/bundles/{generation}";

/// The two array collections and their item routes.
///
/// Each collection needs its prefix separately for the reason the token
/// collection does: [`is_declared_api_route`] has to recognise the item shape
/// with no router to ask.
pub(super) const V1_SSH_KEYS_PATH: &str = "/v1/ssh/authorized-keys";
pub(super) const V1_SSH_KEY_ROUTE: &str = "/v1/ssh/authorized-keys/{fingerprint}";
pub(super) const V1_WIFI_NETWORKS_PATH: &str = "/v1/wifi/client/networks";
pub(super) const V1_WIFI_NETWORK_ROUTE: &str = "/v1/wifi/client/networks/{ssid}";

/// The dot-path the WiFi station's known-network list lives at, which every
/// envelope raised about it names.
///
/// The SSH list's is [`SSH_KEYS_PATH`], declared beside the pane that already
/// writes it: one dot-path for one list, whichever surface is writing it.
pub(super) const WIFI_NETWORKS_PATH: &str = "wifi.client.networks";

/// The station role itself: which radio it runs on, and whether it runs.
///
/// A resource route rather than two scalar writes, for the reason the MQTT
/// listener is one: a station moved to another radio is one decision, and
/// `wifi.client.interface` is not on the scalar allowlist at all -- before
/// this route the only way to bind the station to a different radio was to
/// edit the document on the device.
pub(super) const V1_WIFI_CLIENT_PATH: &str = "/v1/wifi/client";
pub(super) const WIFI_CLIENT_PATH: &str = "wifi.client";

/// The scan: POST, because it puts the radio to work.
pub(super) const V1_WIFI_SCAN_PATH: &str = "/v1/wifi/client/scan";

/// The Bluetooth resource: the adapter, its trust list and the pairing verbs.
///
/// The verbs are POST-only for the reason the power actions are: nothing that
/// merely follows a link should start a radio scan or pair with a device.
pub(super) const V1_BLUETOOTH_PATH: &str = "/v1/bluetooth";
pub(super) const V1_BLUETOOTH_DISCOVERY_PATH: &str = "/v1/bluetooth/discovery";
pub(super) const V1_BLUETOOTH_DEVICE_ROUTE: &str = "/v1/bluetooth/devices/{address}";
pub(super) const V1_BLUETOOTH_DEVICE_ACTION_ROUTE: &str =
    "/v1/bluetooth/devices/{address}/{action}";

/// The settings dot-path the Bluetooth subtree lives at.
pub(super) const BLUETOOTH_SETTINGS_PATH: &str = "bluetooth";

/// The access point: its own resource, for the reason the station role is one.
/// `wifi.ap` carries a key, so the read redacts and the write keeps the stored
/// one unless a new one is sent.
pub(super) const V1_WIFI_AP_PATH: &str = "/v1/wifi/ap";
pub(super) const WIFI_AP_PATH: &str = "wifi.ap";

/// The network cluster: the interface map, one interface, and a tunnel's peer collection.
///
/// The prefix is needed separately for the reason the token collection's is:
/// [`is_declared_api_route`] has to recognise all three shapes under it with
/// no router to ask.
pub(super) const V1_NETWORK_PATH: &str = "/v1/network";
pub(super) const V1_NETWORK_IFACE_ROUTE: &str = "/v1/network/{iface}";
pub(super) const V1_NETWORK_PEERS_ROUTE: &str = "/v1/network/{iface}/peers";
pub(super) const V1_NETWORK_PEER_ROUTE: &str = "/v1/network/{iface}/peers/{publicKey}";

/// The settings dot-path the interface map lives at, which every envelope
/// raised about the whole map names.
pub(super) const NETWORK_SETTINGS_PATH: &str = "network";

/// The MQTT resource: the whole `mqtt` subtree, read with the reconciler's
/// live state beside it and written in one call.
///
/// A resource route rather than three scalar writes through
/// `PUT /api/v1/settings/...`, because the listener is one decision: an
/// address and a port that must take effect together. Two writes would restart
/// the broker twice and leave it bound to an address the operator never asked
/// for in between.
pub(super) const V1_MQTT_PATH: &str = "/v1/mqtt";

/// The settings dot-path the MQTT subtree lives at.
pub(super) const MQTT_SETTINGS_PATH: &str = "mqtt";

/// Where the console listens: the `access.web` subtree, read and written
/// whole, because the ports and the HTTPS switch take effect together in one
/// restart of apid.
pub(super) const V1_WEB_PATH: &str = "/v1/web";

/// The console's TLS identity: read and replaced by upload.
pub(super) const V1_WEB_CERTIFICATE_PATH: &str = "/v1/web/certificate";

/// Replace the console's TLS identity with a new self-signed one.
pub(super) const V1_WEB_CERTIFICATE_GENERATE_PATH: &str = "/v1/web/certificate/generate";

/// The settings dot-path the console's listeners live at.
pub(super) const WEB_SETTINGS_PATH: &str = "access.web";

/// The container resource: the declared map, one entry, and the three
/// lifecycle verbs.
///
/// The verbs are POST-only for the reason the power actions are: no `GET`
/// handler exists, so nothing that merely follows a link can stop a container.
pub(super) const V1_CONTAINERS_PATH: &str = "/v1/containers";
pub(super) const V1_CONTAINER_ROUTE: &str = "/v1/containers/{name}";
pub(super) const V1_CONTAINER_ACTION_ROUTE: &str = "/v1/containers/{name}/{action}";

/// The settings dot-path the declared containers live at.
pub(super) const CONTAINERS_SETTINGS_PATH: &str = "container.units";

/// Each root's three spellings as one tuple, for the test that holds them
/// together.
#[cfg(test)]
pub(crate) const SETTINGS_SPELLINGS: (&str, &str, &str) =
    (V1_SETTINGS_PREFIX, V1_SETTINGS_ROUTE, V1_SETTINGS_DOC);
#[cfg(test)]
pub(crate) const STATE_SPELLINGS: (&str, &str, &str) =
    (V1_STATE_PREFIX, V1_STATE_ROUTE, V1_STATE_DOC);

/// The error names micad maps its `SettingsError` onto, and the five rows of
/// the table that name one. The first two are interface-scoped: the fdo
/// vocabulary has no name that separates a missing dot-path or a read-only
/// one from a bad value, so micad coins its own for those and keeps the
/// standard names for everything else.
pub(super) const MICAD_NOT_FOUND: &str = "com.mica.micad1.Error.NotFound";
pub(super) const MICAD_READ_ONLY: &str = "com.mica.micad1.Error.ReadOnly";
pub(super) const FDO_INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
pub(super) const FDO_IO_ERROR: &str = "org.freedesktop.DBus.Error.IOError";
pub(super) const FDO_FAILED: &str = "org.freedesktop.DBus.Error.Failed";

/// The `Retry-After` on the one class that carries it.
pub(super) const RETRY_AFTER_SECONDS: &str = "5";

/// The major versions this build serves — the *served set*, which is an
/// array because it can legitimately have more than one member.
pub(super) const SERVED_VERSIONS: [&str; 1] = ["v1"];

/// The member of the served set a client with no preference should use.
pub(super) const CURRENT_VERSION: &str = "v1";
