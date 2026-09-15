# micad

`micad` is the management-plane daemon (crate `crates/micad`, package
`micad`). It owns the device's settings, converges the system to them, and is
the single D-Bus authority every management client talks to.

## 1. Responsibilities

- Load and persist the settings tree; refuse, rather than guess, when a
  document or the medium holding it is unusable.
- Run the reconcilers that turn settings into system configuration and unit
  state.
- Keep the live-state tree and the read-only observers.
- Provision a device on first boot and from offline provisioning documents.
- Execute power actions, reset tiers and physical recovery intents.
- Drive signed updates through `mica-deploy` under the operator's policy.
- Maintain the registry of other `com.mica.*` services on the bus.

micad never listens on a network socket. Everything reaches it through the
system bus, and only root may call it.

## 2. Startup

`crates/micad/src/lib.rs` is the entry point. In order:

1. `--version`/`-V` is answered before anything else is read or written.
2. The environment is read (below) and the settings store opened.
3. If the DATA medium holding `/mica/config` is unavailable, micad refuses to
   start and names the mount (`RequiresMountsFor=/var/lib/mica /mica` on the
   unit prevents a race with a slow disk).
4. Each configuration document is loaded; a document that does not parse is
   logged at ERROR with its file name and disables only the subtrees it carries.
5. First-boot provisioning runs when STATE holds no configuration, and a staged
   provisioning document is applied.
6. The reconcilers are constructed, the service scan starts, the bus name is
   claimed (`Type=dbus`), and every reconciler is applied once.

| Variable | Default | Meaning |
| --- | --- | --- |
| `MICAD_SETTINGS_PATH` | `/var/lib/mica/settings.toml` | The STATE document |
| `MICAD_CONFIG_DIR` | `/mica/config` | The configuration namespace on DATA |
| `MICAD_BUS` | `system` | `system` or `session` |
| `MICAD_SHADOW_PATH` | `/etc/shadow` | Where a transient root password is written |
| `MICAD_META_MANIFEST_PATH` | `/usr/share/mica/meta/updates/manifest.json` | The baked update configuration in the read-only root |
| `MICAD_UPDATE_POLICY_PATH` | `/mica/config/updates.json` | The operator's update policy |
| `MICAD_PROVISIONING_ROOT` | `/run/mica/provisioning` | Where offline provisioning media are staged |
| `MICAD_DRY_RUN` | unset | `1`: no provisioning, no reconcilers, no host access; used by tests |
| `MICAD_SCAN` | unset | Test hook enabling the service scan under dry run |

## 3. Settings

### 3.1 Model

`crates/micad-settings/src/model.rs` defines `Settings`, the one tree every
reader sees:

| Subtree | Contents |
| --- | --- |
| `hostname` | The system hostname |
| `network.<iface>` | Per-interface configuration; `kind` is `physical`, `vlan`, `bridge` or `wireguard`, with the one block that belongs to it (static/DHCP addressing, VLAN parent and ID, bridge ports, WireGuard peers) |
| `access` | `ssh` (enabled, listen addresses, authorized keys), `console`, `webAdmin` (password hash), API tokens, claim and device-credential state |
| `wifi` | `client` (known networks) and `ap` (access point) |
| `container` | The container engine switch |
| `mqtt` | The switch for broker and bridge, the broker listen address and port (default `127.0.0.1:1883`), whether clients must authenticate |
| `time` | NTP servers and the presentation timezone |
| `provisioning` | First-boot and provisioning-document status |
| `reset` | A staged reset intent, present only while one is waiting |

Validation is total: a write is checked against the whole tree (for example a
VLAN's parent must be a declared entry) before anything is persisted. The
interface name is a convention; the `kind` block is authoritative.

### 3.2 Storage

| Document | Location | Carries |
| --- | --- | --- |
| `system.json` | `/mica/config/` (DATA) | `hostname`, `access.console` |
| `network.json` | `/mica/config/` | `network` |
| `wifi.json` | `/mica/config/` | `wifi` |
| `ssh.json` | `/mica/config/` | `access.ssh` |
| `mqtt.json` | `/mica/config/` | `mqtt` |
| `time.json` | `/mica/config/` | `time` |
| `container.json` | `/mica/config/` | `container` |
| `settings.toml` | `/var/lib/mica/` (STATE) | identity, credentials, provisioning state, staged reset |

The split follows the reset tiers: tier 1 (configuration) clears exactly what
lives in `/mica/config/`. Each document has its own schema version; unknown
keys and other versions are refused without conversion. Writes go through a
private, fsynced undo journal, so a power loss restores the previous documents
rather than leaving a half-written set.

`crates/micad-settings/src/configuration.rs` owns `updates.json` and the baked
manifest it overrides, because apid reads the same two documents through the
same resolver.

## 4. Reconcilers

Contract (`crates/micad/src/reconciler/mod.rs`): a stable `name` (its key
in the live-state tree), the `subtree` dot-path it watches, and `apply`, which
converges the system to the settings and returns the applied state as JSON.
`reconciler::all()` lists the production set:

| Reconciler | Renders | Drives |
| --- | --- | --- |
| `hostname` | `/etc/hostname` | the running hostname via hostnamed |
| `network` | `/run/systemd/network/*.network`, `*.netdev`; WireGuard private keys (generated on the device, never in settings) | systemd-networkd reload |
| `sshd` | `/run/mica/dropbear.env` (`DROPBEAR_ARGS`), `~/.ssh/authorized_keys` of `root` and `mica` | `dropbear.service` restart when changed |
| `wifi_client` | wpa_supplicant configuration, the link's networkd unit | `wpa_supplicant@<iface>.service` |
| `wifi_ap` | hostapd configuration, the AP's networkd unit with its DHCP server | `hostapd@<iface>.service` |
| `container` | the mount unit binding `/etc/containers/systemd` from STATE | systemd daemon-reload, so Quadlet units exist only while enabled |
| `mqtt` | `/run/mica/mqtt-broker.toml`, `/run/mica/mqttd-device.env` | `mica-mqtt-broker.service`, `mica-mqttd.service` |
| `time` | `/run/systemd/timesyncd.conf.d/60-mica-servers.conf`, `/run/mica/timezone` | systemd-timesyncd |

Shared helpers: `reconciler/systemd.rs` (unit start/stop/restart/reload,
`reset-failed`) and `fswrite.rs` (writing into STATE-backed bind mounts
correctly).

### 4.1 Apply tasks

`SetSettings` persists the value first, then enqueues an apply job for the
reconcilers whose subtree overlaps the written path (`apply_queue.rs`). The
call returns the task id; progress is published as `TaskChanged` and readable
with `GetTask`. The queue keeps a bounded history and carries only the
operation and the dot-path, never setting values.

## 5. D-Bus interface

Bus name `com.mica.micad`, object `/com/mica/micad`, interface
`com.mica.micad1`. Structured values are JSON strings.

### 5.1 Settings, state and tasks

| Member | Arguments → result | Purpose |
| --- | --- | --- |
| `GetSettings` | `path` → JSON | Read the settings tree at a dot-path (`""` for all) |
| `SetSettings` | `path`, `value_json` → task id | Validate, persist and apply one value |
| `GetTask` | `id` → JSON | One apply task |
| `GetState` | `path` → JSON | Read the live-state tree |
| `ReportHealth` | `component`, `status`, `detail` | Record a component health report (used by the boot health gate) |
| `ForgetService` | `bus_name` | Drop a vanished service from the registry |
| `SettingsChanged` (signal) | `path`, `value_json` | A settings write succeeded |
| `TaskChanged` (signal) | `task_json` | An apply task moved |

### 5.2 Observation

| Member | Result |
| --- | --- |
| `GetNetworkState` | Links, addresses and routes as networkd, wpa_supplicant and resolved report them |
| `GetObservedNetwork` | The observed network view the dashboard shows |
| `GetTimeStatus` | timesyncd synchronisation state |
| `GetStorageStatus` | Storage tiers, bind namespaces, usage, quotas, media health |
| `GetSystemInfo` | What the device is, assembled from where each fact already lives |
| `GetTelemetry` | Board temperature, watchdog and reset reason |
| `GetFailureEvidence` | Failed units and a bounded journal excerpt |

### 5.3 Actions

| Member | Arguments | Purpose |
| --- | --- | --- |
| `Reboot`, `PowerOff` | — | Power actions through systemd, recorded with the caller |
| `SetTransientRootPassword` | `password` | A root password that lasts until the next boot |
| `RotateWireguardKey` | `iface` → the new public key | Draw a new WireGuard private key; the reconcilers re-render the tunnel |
| `GetUpdateState` | → JSON | The complete update state |
| `CheckUpdate`, `FetchUpdate` | — | Check the signed catalog; download the selected deployment |
| `InstallUpdate` | `deployment_id` | Install an acquired deployment |
| `ConfirmDeployment`, `RejectDeployment`, `RollbackDeployment` | `deployment_id` | Boot lifecycle actions |
| `SetRebootOverride` | `seconds` → JSON | Bounded administrative override of the safe-to-reboot gate |
| `SetUpdateConfig` | `patch_json` → JSON | Write the operator update policy |

Errors are D-Bus errors; a value the tree rejects is
`org.freedesktop.DBus.Error.InvalidArgs`.

### 5.4 Access control

`crates/micad/dist/com.mica.micad.conf` makes `com.mica.micad` root-only
in both directions: non-root callers may neither send to it nor receive its
signals, because `SettingsChanged` carries setting values such as password
hashes. `mica-mqttd` has no exception.

## 6. Provisioning and identity

- **First boot** (`provisioning.rs`, `identity.rs`): when STATE holds no
  configuration, micad generates the device identity and per-device
  credentials on the device (the root image is identical across the fleet, so
  nothing secret can be baked in), persists them, and brings the device up in a
  working default configuration. This touches no network.
- **Provisioning documents** (`provisioning_doc.rs`): a versioned file on the
  boot medium or removable media configures a device with no network. The whole
  document is validated before anything is written, applied atomically, and
  recorded so reapplying it is a no-op. apid reports the result at
  `GET /api/v1/provisioning/status`.

## 7. Updates

- `deployment.rs` runs `/usr/bin/mica-deploy` with bounded time and output and
  parses its JSON status.
- `update_policy.rs`: refusals, the maintenance window, the automatic check
  cadence and the safe-to-reboot gate.
- `update_lifecycle.rs`: records acquisition and install progress and applies
  the maintenance and reboot gates to actions.
- `update_auto.rs`: automatic check, download and install within the operator
  policy; failed deployment IDs and generation floors prevent reinstalling a
  rejected release.
- `update_codes.rs`: the closed set of machine-readable failure and deferral
  codes.

## 8. Power, reset and recovery

- `power.rs`: reboot and power-off through systemd; actions, not settings.
- `reset.rs`: applies a staged reset tier — `configuration` (settings back to
  schema defaults), `application-data` (operator applications and their data),
  `full-factory` (first-boot state, keeping identity, calibration, META and both
  system slots). apid stages the intent; micad applies it.
- `recovery.rs`: reads a board-declared physical recovery action at boot
  (`/usr/lib/mica/recovery-actions.conf`) and maps it to a presence assertion
  and a reset tier.
- `transient.rs`: the transient root password, written to the shadow file and
  cleared on the next boot.

## 9. Service registry

`scan.rs` watches `NameOwnerChanged` for other `com.mica.*` names and records
what it finds in the live-state tree. The name rule — how `com.mica.<class>`
becomes a class — is `crates/mica-busname`, shared with the MQTT bridge. The scan
is passive: a match rule and read-only calls.

## 10. The unit

`micad.service`: `Type=dbus`, `BusName=com.mica.micad`, root, restarted on
failure. It keeps root and broad file access (it writes `/etc/shadow`,
`/run/mica`, `/run/systemd/network` and the managed accounts' home
directories) and is otherwise confined: `NoNewPrivileges`, `PrivateTmp`,
kernel and cgroup protection, no realtime, no SUID/SGID, no writable-executable
memory, native system calls only, and only `AF_UNIX`, `AF_NETLINK`, `AF_INET`
and `AF_INET6` sockets.
