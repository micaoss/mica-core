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
| `container` | The container engine switch, and `units`: the containers this device declares |
| `bluetooth` | The adapter switch, whether it is discoverable, its advertised name, the pairing code and `devices`: the trust list |
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
| `bluetooth.json` | `/mica/config/` | `bluetooth` |
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
| `wifi_client` | wpa_supplicant configuration, the link's networkd unit. A passphrase network is `key_mgmt=WPA-PSK SAE` with `ieee80211w=1`, so one block joins WPA2, WPA3 and transition-mode access points; a network stored as a raw PMK is WPA2 only, since SAE needs the password; `sae_pwe=2` admits hash-to-element, which WPA3 on 6 GHz requires | `wpa_supplicant@<iface>.service` |
| `wifi_ap` | hostapd configuration, the AP's networkd unit with its DHCP server | `hostapd@<iface>.service` |
| `container` | the mount unit binding `/etc/containers/systemd` from STATE, and a `50-mica-<name>.container` per declared container | systemd daemon-reload, so Quadlet units exist only while enabled; each declared unit to the state its `autoStart` asks for |
| `mqtt` | `/run/mica/mqtt-broker.toml`, `/run/mica/mqttd-device.env` | `mica-mqtt-broker.service`, `mica-mqttd.service` |
| `time` | `/run/systemd/timesyncd.conf.d/60-mica-servers.conf`, `/run/mica/timezone` | systemd-timesyncd |
| `bluetooth` | nothing: BlueZ owns the adapter, and this reconciler sets its properties | `bluetooth.service`, the adapter's `Powered`/`Discoverable`/`Alias`, and each declared device's trust |

### 4.1 Apply tasks

`SetSettings` persists the value first, then enqueues an apply job for the
reconcilers whose subtree overlaps the written path (`apply_queue.rs`). The
call returns the task id; progress is published as `TaskChanged` and readable
with `GetTask`. The queue keeps a bounded history and carries only the
operation and the dot-path, never setting values.

### 4.2 What the table above is measured against

Each cell, measured. Both subtrees are the reconciler's own name:
`"container"` (`crates/micad/src/reconciler/container.rs`) and
`"mqtt"` (`crates/micad/src/reconciler/mqtt.rs`). The container
executor is not a daemon — the engine is daemonless and the image carries no
podman unit — so what the reconciler operates is the mount that makes Quadlet's
directory readable, `pub const QUADLET_MOUNT_UNIT: &str = "etc-containers-systemd.mount";`
(`crates/micad/src/reconciler/container.rs`), followed by
`self.control.daemon_reload().await?;`
(`crates/micad/src/reconciler/container.rs`), without which the mount
is correct, the files are visible and no unit exists. The MQTT executor writes
`const DEFAULT_CONFIG_PATH: &str = "/run/mica/mqtt-broker.toml";`
(`crates/micad/src/reconciler/mqtt.rs`) and drives two units,
`const BROKER_UNIT: &str = "mica-mqtt-broker.service";`
(`crates/micad/src/reconciler/mqtt.rs`) and
`const BRIDGE_UNIT: &str = "mica-mqttd.service";`
(`crates/micad/src/reconciler/mqtt.rs`).

**The `SshdReconciler` subtree contract:**
`SshdReconciler` watches **`access.ssh` and nothing else**. `access.device` is
**deliberately not watched.** A reader who finds the credential subtree missing
should not reconstruct it as an oversight and add it back.

This reconciler **does not write `/etc/shadow`**. It
reads the marker beside it to decide whether password authentication may be
offered (`access.md` §3.1), but the only writers of that file today are micad's
transient-password bus method and `mica-shadow-reconcile` at boot.

Every reconciler follows the same discipline:

- **pure render, then compare, then write.** The render is a deterministic
  function of the subtree; `apply` re-renders, compares against what is on disk,
  and skips the write when the bytes match. These files live on DATA/state, so an
  unconditional rewrite costs a flash write on every reconcile.
- **read live state before acting.** Ask systemd for the unit's `ActiveState` and
  unit-file state first, and issue only the calls that change something. A
  converged system produces **zero** bus calls.
- **restart on config change.** The one case that must not be a no-op: a daemon
  that reads its configuration once at start, whose file changed under it, is
  restarted. Otherwise the rewrite silently did not take effect.
- **enablement is runtime-scoped** (`EnableUnitFiles` with `runtime = true`).
  Persistent enablement needs `/etc/systemd/system` to be writable, and on the Mica OS
  read-only root it is not — a persistent enable would fail with EROFS on device
  while passing every test on a normal filesystem. micad reconciles the whole tree
  at every start, so units return to their configured state each boot anyway.
- **outcomes are named, never boolean** — `applied`, `unchanged`, `idle`,
  `disabled`, `stopped`, `conflict`, `absent`, `plaintext-missing`. Several of
  these are skips, and confusing two of them is how a broken image gets reported
  as a healthy one.
- **secrets reach the config file and nothing else** — not the live-state tree
  (which is served over D-Bus), not a log line, not an error message.

The settings/live-state split carries all of this: each reconciler publishes its status onto the live-state tree, which apid
reads over the bus.

### 4.3 The network reconciler: kinds, netdevs and teardown

Everything above still holds for a physical interface: one `50-mica-<iface>.network`
file, rendered, compared, swept. What schema v7 added is a `kind` on each
`network` entry — physical, `vlan`, `bridge` or `wireguard` — and three things
the reconciler has to do that a `.network` file alone cannot express.

**A virtual link needs a `.netdev` as well.** The renderer is a second function
beside the unit renderer: *"Render the `.netdev` unit that creates `iface`, for
a kind that needs one"* (`crates/micad/src/reconciler/network.rs`),
answering *"`None` for a physical entry, whose device the kernel already has"*
(`crates/micad/src/reconciler/network.rs`). A VLAN's netdev carries
`Kind=vlan` and its `[VLAN] Id=`, a bridge's `Kind=bridge`, and a tunnel's
`Kind=wireguard` plus
*"the `[WireGuard]` and `[WireGuardPeer]` sections of a tunnel's netdev"*
(`crates/micad/src/reconciler/network.rs`).

**Attachment is a line on the OTHER interface's unit.** A VLAN child is named
by its parent and a bridge port by nothing of its own, because
*"networkd creates a VLAN only when the parent's `.network` names it"*
(`crates/micad/src/reconciler/network.rs`) — so the child's
existence is a fact the PARENT's unit has to state, and `render_unit` takes the
parent's VLAN children and the bridge that claimed this interface as arguments
rather than reading them off the entry. A port carries no addressing:
*"A port's addressing is the bridge's; validation has already refused an entry
that tried to keep its own"*
(`crates/micad/src/reconciler/network.rs`). Both relations are
fail-closed before a single file is written — *"an undeclared parent is a VLAN
that would never come up"*
(`crates/micad/src/reconciler/network.rs`) — which is the same
boundary argument the address validator makes: the settings file is writable
without apid.

**The sweep grew a teardown, because deleting a file is not deleting a device.**
The sweep still deletes every `50-mica-` unit the pass did not write, now over
both suffixes — *"Whether `file_name` is one this reconciler wrote:
`50-mica-<iface>.network` or, for a virtual link, `50-mica-<iface>.netdev`"*
(`crates/micad/src/reconciler/network.rs`) — and it then asks the
kernel to drop the device, because *"Removing a `.netdev` file and reloading
does not delete the device networkd built from it: networkd creates virtual
devices, it does not reap them"*
(`crates/micad/src/reconciler/network.rs`). The same delete covers a
netdev whose properties changed: *"Devices whose netdev properties changed. They
apply at creation only, so the device has to go and be built again"*
(`crates/micad/src/reconciler/network.rs`). The deletes run
*"Before the reload, so networkd builds the recreated devices back on the same
pass that deleted them"* (`crates/micad/src/reconciler/network.rs`),
and a failed delete in the sweep is logged rather than returned — the unit file
is already gone and failing there would report every converged interface as
unconverged.

**A WireGuard private key never enters the settings tree.** The schema is
explicit that it never will: *"There is no private-key field here and there
never will be"* (`crates/micad-settings/src/model.rs`). The key lives
in a file under the DATA/state directory that holds `settings.toml`, in
`networkd-secrets/` — *"A sibling of `secrets/` rather than anything under it,
and the name says so because the path is load-bearing"*
(`crates/micad/src/wgkeys.rs`), a sibling and not a child because the
identity module pins `secrets/` to 0700 on every pass and nothing below a 0700
directory is traversable by the `systemd-network` user. The modes follow from
who reads it: *"the key file is `root:systemd-network` 0640 under a sibling
directory of the same ownership at 0750"*
(`crates/micad/src/wgkeys.rs`). Generation is lazy and idempotent —
*"Idempotent: an interface that already has a key keeps it, so a reconcile pass
never rotates by accident"* (`crates/micad/src/wgkeys.rs`) — and
each write is the store's usual shape: *"temp file beside the target, fsync,
rename, fsync the directory"* (`crates/micad/src/wgkeys.rs`), with
mode and group set on the temp file before the rename. The rendered unit names
the file rather than carrying the key: *"`PrivateKeyFile=` names the key rather
than carrying it"* (`crates/micad/src/reconciler/network.rs`), which
matters because the netdev sits in networkd's world-readable runtime directory.
Only the public half is ever published, into the live-state entry —
`entry["publicKey"] = json!(self.keys.ensure(iface)?);`
(`crates/micad/src/reconciler/network.rs`) — beside the `file`, `dhcp`
and `kind` keys every entry carries: `"kind": kind_name(cfg.kind),`
(`crates/micad/src/reconciler/network.rs`).

This is the reconciler discipline's *"secrets reach the config file and nothing
else"* rule applied to a secret the config file may not hold either: the key
reaches its own file, and the module carries no logging statement at all.

**Rotation is a bus method, not a settings write.** `RotateWireguardKey(iface)`
answers the new public key, and it is a method for the reason the transient root
password is: *"Deliberately not a setting, for the reason a transient root
password is not one: a key that reached the settings tree would be persisted and
served back out of it"* (`crates/micad/src/bus.rs`). It refuses an
interface that is not a declared `network` entry of kind `wireguard`, runs under
the same lock every mutating method takes, and then re-reconciles:
*"The reconcilers are re-run afterwards so the tunnel's unit is re-rendered and
networkd builds the device back around the key now on disk"*
(`crates/micad/src/bus.rs`). The re-run is not optional, because
*"networkd reads `PrivateKeyFile=` when it creates the device and never again"*
(`crates/micad/src/reconciler/network.rs`) — a rotation that only
rewrote the file would change what the public key says without changing what the
tunnel uses. No `SettingsChanged` is emitted: nothing in the settings tree
changed.

### 4.4 Bluetooth, and the agent that pairs

BlueZ owns the adapter; micad owns what is declared about it. `bluetooth`
carries the switch, whether the adapter answers scans, the advertised name, the
pairing code and `devices` -- the trust list, by address.

The reconciler brings `bluetooth.service` to the switch, sets the adapter's
properties, reconciles each declared device's trust, and **removes a paired
device the settings tree does not name**. Only a paired one: BlueZ publishes an
object for every device it has merely seen, and sweeping those would delete the
results of the scan an operator is looking at.

**A board with no radio reports `unsupported`**, not a failure. It is the only
reconciler here whose subject is optional hardware, and one that failed would
fail on every pass forever.

Pairing is not a reconcile. micad registers an `org.bluez.Agent1` at
`/com/mica/bluetooth/agent` with capability `DisplayYesNo`; when BlueZ asks for
a confirmation the agent records the passkey and **blocks its reply** until
`ConfirmBluetoothPairing` answers or 45 seconds pass. One request at a time: two
passkeys on one screen is two decisions an operator cannot tell apart.

A legacy peer that asks for a code is answered with `bluetooth.pin`. That value
is **displayed** in the console by design -- somebody has to type it on the
other device -- and an absent one is derived from the device identity rather
than defaulting to a constant, so a fleet does not share one code. What bounds
legacy pairing is not the code but discovery: it is an operator action with a
timeout.

### 4.5 Declared containers

`container.units` is a map of container names to the fields a Quadlet
`.container` unit needs: image, command, environment, published ports, volumes,
restart policy and whether it starts at boot. The reconciler renders each entry
to `50-mica-<name>.container` in the bound Quadlet directory, compares before
writing, sweeps the `50-mica-` files it no longer declares -- and only those, so
a `.container` an integrator dropped in by hand is left alone -- and reloads so
Quadlet regenerates. A **bridge-port-like** rule applies to volumes: a host path
must be under `/mica/`, checked by `micad-settings` itself because the document
is writable without apid.

Lifecycle is systemd's. `StartContainer` and its siblings drive
`50-mica-<name>.service`, never podman: a container podman started is one no
unit name can stop and nothing brings back after a reboot.

What the engine reports is read separately (`containers.rs`): `podman ps --all`
and `podman images`, bounded in time and output, **read-only** -- no `run`, no
`rm`, no `pull`. Starting a unit pulls the image if it has to, which keeps the
one writer of container state the unit file.

Shared helpers: `reconciler/systemd.rs` (unit start/stop/restart/reload,
`reset-failed`) and `fswrite.rs` (writing into STATE-backed bind mounts
correctly).

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
| `GetStorageStatus` | Storage tiers, bind namespaces, usage, quotas, media health. The tiers are found from the signed boot policy (`/run/mica/boot-policy.json`): SYSTEM and DATA by partition UUID, the boot partition (`esp` or `firmware`) by its GPT number on the same disk, and each is reported under the name the disk gives it. GPT names are only the fallback where no policy is readable |
| `GetSystemInfo` | What the device is, assembled from where each fact already lives |
| `GetTelemetry` | Board temperature, watchdog and reset reason |
| `GetFailureEvidence` | Failed units and a bounded journal excerpt |
| `GetContainers` | The declared container map joined with what the engine reports, each side named |
| `ScanWifi` | What the station's radio finds on the air: one entry per network with its SSID, BSSID, signal and flags |
| `GetBluetooth` | The declared trust list, the adapter, the devices BlueZ holds, the pairing code and whatever is waiting to be confirmed |

### 5.3 Actions

| Member | Arguments | Purpose |
| --- | --- | --- |
| `Reboot`, `PowerOff` | — | Power actions through systemd, recorded with the caller |
| `SetTransientRootPassword` | `password` | A root password that lasts until the next boot |
| `RotateWireguardKey` | `iface` → the new public key | Draw a new WireGuard private key; the reconcilers re-render the tunnel |
| `GetUpdateState` | → JSON | The complete update state |
| `CheckUpdate`, `FetchUpdate` | — | Check the signed catalog; download the selected deployment |
| `ImportUpdate` | `path` | Import an offline `MICAUPD1` archive apid streamed into the workspace's `uploads/`; a path outside it is refused |
| `InstallUpdate` | `deployment_id` | Install an acquired deployment |
| `ConfirmDeployment`, `RejectDeployment`, `RollbackDeployment` | `deployment_id` | Boot lifecycle actions |
| `SetRebootOverride` | `seconds` → JSON | Bounded administrative override of the safe-to-reboot gate |
| `SetUpdateConfig` | `patch_json` → JSON | Write the operator update policy |
| `StartContainer`, `StopContainer`, `RestartContainer` | `name` | systemd verbs on the unit Quadlet generated for a declared container; a name the settings tree does not hold is refused |
| `SetBluetoothDiscovery` | `on` | Start or stop a scan; refused with the switch off or with no adapter |
| `PairBluetoothDevice` | `address` | Pair, then record the device in the trust list as trusted |
| `ConfirmBluetoothPairing` | `address`, `accept` | Answer the passkey the agent is holding |
| `RemoveBluetoothDevice` | `address` | Drop the declaration and tell the adapter to forget its keys |

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
  cadence and the safe-to-reboot gate. The cadence is `checkIntervalMinutes`
  measured from the daemon's start, unless the document names `checkAt`
  (`HH:MM` UTC): then the check is that time of day, answered once per
  crossing, seeded at the driver's start so an anchor crossed while the device
  was down does not fire at boot. An unbelieved clock falls back to the
  interval rather than stopping -- a clockless device must keep discovering
  updates.
- `update_lifecycle.rs`: records acquisition and install progress and applies
  the maintenance and reboot gates to actions.
- `update_auto.rs`: automatic check, download and install within the operator
  policy; failed deployment IDs and generation floors prevent reinstalling a
  rejected release.
- `update_codes.rs`: the closed set of machine-readable failure and deferral
  codes.

## 8. Power, reset and recovery

- `power.rs`: reboot and power-off through `org.freedesktop.systemd1.Manager`;
  actions, not settings. Each resolves the caller's unique bus name and **logs
  the action with its source and records it in live state before invoking the
  power control** — after the call there may be no system left to log on. The
  record is live state under `power` (`{ last_action, requested_by }`), and the
  settings documents, `SCHEMA_VERSION` included, are left as they were.
- `reset.rs`: applies a staged reset tier — `configuration` (settings back to
  schema defaults), `application-data` (operator applications and their data,
  `container.units` among them: a declared container is an operator
  application),
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
