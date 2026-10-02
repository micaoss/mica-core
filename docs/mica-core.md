# mica-core

mica-core is the management plane of Mica OS: the software that owns a device's configuration,
turns it into running services, exposes it to operators, drives signed updates, and boots and
shuts the device down around a signed, read-only root. This document describes it as it is: the
packages and what they run, the daemons and their contracts with the rest of the system, and how
the repository is built, checked and released.

## 1. Where it sits

| Repository | Provides | Relationship |
| --- | --- | --- |
| `micaoss/mica-build-env` | The build images | Every build and gate here runs in them (`locks/mica-build-env.lock`) |
| `micaoss/mica-build-tools` | The lock, packing and release rules | Pinned by commit (`locks/mica-build-tools.pin`), run through `bin/mica-tools` |
| `micaoss/mica-system-base` | The floor root, both inits and the option packages (SSH, Wi-Fi, Bluetooth, time zones) | mica-core's packages run on it and drive its services by name (section 3.8) |
| `micaoss/mica-podman` | The container engine and its supervisor, mica-containerd | micad's container reconciler starts mica-containerd and declares the containers to it |
| `micaoss/mica-build` | Boards, products and the signed images | Pins a mica-core release, installs its packages into each product's root and signs its core components into each deployment |

## 2. Core components and packages

Two things ship as **core components** of a product's deployment, not as Debian packages: they
are composed over the root at boot, so a
mica-core release reaches devices without a new root. Each is a squashfs of its `/usr` and `/etc`
with an appended dm-verity hash tree, and an unsigned `mica/core/v1` record naming it; both are
layers of the release's pool, pool items of the types `core.img` and `core.json` named by `item`
rows of `mica-core.lock` (`mica-build-tools:docs/spec/release-lock.md` 1.2.7). mica-build signs the
root hash with the product's key.

| Component | What it holds | systemd | OpenRC | Started |
| --- | --- | --- | --- | --- |
| `micad` | The management daemon and its D-Bus policy, and the API: `/usr/bin/mica-apid`, a link to `micad`, with the OpenAPI document | `micad.service`, `apid.service` | `/etc/init.d/micad`, `/etc/init.d/apid` | At boot (the component links both) |
| `mica-apid-ui` | The web console at `/usr/share/mica-apid/ui`; feature `ui`, at micad's exact version | -- | -- | Served by apid; optional |

The rest are Debian packages built into the root:

| Package | What it installs | systemd | OpenRC | Started |
| --- | --- | --- | --- | --- |
| `mica-mqtt-broker` | The local MQTT 3.1.1 broker | `mica-mqtt-broker.service` | `/etc/init.d/mica-mqtt-broker` | By micad, when MQTT is on |
| `mica-mqttd` | The application-data bridge to MQTT | `mica-mqttd.service` | `/etc/init.d/mica-mqttd` | By micad, when MQTT is on |
| `mica-sftp-server` | `/usr/lib/sftp-server`, exec'd by dropbear | -- | -- | Per SSH session |
| `mica-deploy` | The deployment client | -- | -- | By micad and the boot health gate |
| `mica-lifecycle` | `mica-runkit`, static, which the assembly packs into the signed kernel | -- | -- | PID 1 of the initramfs and the exit ramdisk |

**Each component and package carries the start-up files of its own services, for both inits, and
none of another's.** micad and apid enable themselves: a `multi-user.target.wants` link and an
`/etc/runlevels/default` link each, in the micad component (mica-openrc keeps runlevel links it
finds). The MQTT daemons are enabled on neither init;
micad starts them after writing their configuration.

On OpenRC the scripts run under `supervise-daemon` with their output in syslog, and
`/etc/init.d/micad` reports micad started only once it owns `com.mica.micad` on the system bus
(waiting up to `micad_ready_timeout`, 90 s), which is what `Type=dbus` does under systemd, and
`/etc/init.d/apid` only once `mica-apid --healthcheck` answers (up to `apid_ready_timeout`, 90 s),
so the health gate that runs after every service never asks an apid still making its first TLS
identity.

## 3. micad

`crates/micad`, root, no network listener. It loads and persists the settings, runs the
reconcilers that turn them into system configuration, keeps the live-state tree and the
read-only observers, provisions the device, executes power, reset and recovery, drives updates
through `mica-deploy`, and keeps a registry of the other `com.mica.*` services on the bus.

### 3.1 Startup

In order: `--version` is answered before anything is read; the settings are loaded, and micad
refuses to start when the DATA medium holding `/mica/config` is not there (a device that cannot
read its configuration must not render another); a staged reset and a boot recovery intent are
applied; a staged provisioning document is imported; first-boot provisioning mints identity and
credentials; the product file is read; the reconcilers are built; every reconciler is applied
once; the bus name is claimed.

| Variable | Default | Meaning |
| --- | --- | --- |
| `MICAD_SETTINGS_PATH` | `/var/lib/mica/settings.toml` | The STATE document |
| `MICAD_CONFIG_DIR` | `/mica/config` | The configuration documents on DATA |
| `MICAD_BUS` | `system` | `system` or `session` |
| `MICAD_SHADOW_PATH` | `/etc/shadow` | Where a transient root password goes |
| `MICAD_META_MANIFEST_PATH` | `/usr/share/mica/meta/updates/manifest.json` | The baked update configuration |
| `MICAD_UPDATE_POLICY_PATH` | `/mica/config/updates.json` | The operator's update policy |
| `MICAD_PROVISIONING_ROOT` | `/run/mica/provisioning` | Where offline provisioning media are staged |
| `MICAD_DRY_RUN` | unset | `1`: no provisioning, no reconcilers, no host access (the tests) |

### 3.2 The product

`/usr/lib/mica/product.conf`, written when the root is composed and authenticated with it:

- `PRODUCT` names the product a deployment must be built for.
- `FEATURES` lists what the product carries: `wifi`, `bluetooth`, `ssh`, `containers`, `mqtt`.
  The reconciler, the bus members and the API routes of a feature not listed do not exist on
  the device; a write to its subtree is refused as `feature not in this product`, and its routes
  answer 404. No file, or no `FEATURES` line, serves every feature.
- `INIT` is `systemd` (the default) or `openrc`, and decides every backend below. An unknown
  init stops micad.

### 3.3 Settings

One typed tree (`crates/micad-settings`), addressed by dot-paths:

| Subtree | Contents |
| --- | --- |
| `hostname` | The hostname |
| `network.<iface>` | `kind` (`physical`, `vlan`, `bridge`, `wireguard`) and its block, DHCP or static addressing, routes, a DHCP server |
| `access` | SSH (switch, listen addresses, authorized keys), the console, the admin password hash, API tokens, claim state |
| `wifi` | `client` (known networks) and `ap` (the access point) |
| `container` | The engine switch and the declared `units` (image, command, environment, ports, volumes under `/mica/`, restart, start at boot, and the limits `pids`, `memory`, `cpu`) |
| `bluetooth` | The switch, discoverability, the name, the pairing code, the trust list |
| `mqtt` | The switch, the broker listener (default `127.0.0.1:1883`), whether clients authenticate |
| `time` | NTP servers and the presentation timezone |
| `provisioning`, `reset` | Provisioning status; a staged reset |

What an integrator sets lives on DATA, one JSON document per concern in `/mica/config/`
(`system`, `network`, `wifi`, `ssh`, `mqtt`, `time`, `container`, `bluetooth`, directory `0700`,
documents `0600`), beside `updates.json`; what the device mints (identity, credentials,
provisioning state, a staged reset) is `settings.toml` on STATE. Each document has its own
schema version, and an unknown key or version is refused, never converted. A document that does
not parse disables only the subtrees it carries. Writes are validated against the whole tree,
go through a fsynced undo journal, and are applied as a task (`SetSettings` returns its id;
`TaskChanged` reports it).

A container's limits are sent only when declared: `pids` (1 to 65536), and when absent the
container keeps podman's own 2048; `memory` (`<n>k|m|g`, at least 6m) and `cpu` (a number of
CPUs, such as `0.5`), unlimited when absent.

### 3.4 Reconcilers

Each owns one subtree and converges the system to it, returning the applied state into the
live-state tree under its name.

| Reconciler | systemd | OpenRC |
| --- | --- | --- |
| `hostname` | `/etc/hostname`, hostnamed | `/etc/hostname`, `sethostname(2)` |
| `network` | networkd `.network`/`.netdev` under `/run/systemd/network` for every kind; WireGuard keys generated on the device and named by file | `/var/lib/mica/network/interfaces` for busybox ifupdown (`ifdown -a` on the old file, `ifup -a` on the new): DHCP, static IPv4/IPv6 with DNS into `/run/mica/resolv.d/<iface>`, or no addressing; undeclared `eth*` get DHCP. VLAN, bridge, WireGuard, routes and the DHCP server are refused by name |
| `sshd` | `/run/mica/dropbear.env`, the authorized keys of `root` and `mica`, `dropbear.service` | the same files, `mica-dropbear` |
| `wifi_client` | wpa_supplicant configuration in `/etc/wpa_supplicant`, `wpa_supplicant@<if>`, networkd DHCP | configuration in `/var/lib/mica/wpa_supplicant`, `/run/mica/wifi-client.env`, `mica-wifi-client` |
| `wifi_ap` | hostapd configuration in `/etc/hostapd`, `hostapd@<if>`, networkd address and DHCP server | configuration in `/var/lib/mica/hostapd`, `/run/mica/wifi-ap.env`, busybox udhcpd on the same pool (`/run/mica/wifi-ap-udhcpd.conf`), `mica-wifi-ap` |
| `container` | Starts `mica-containerd.service` and declares every container in one `PUT /v1/containers`; switched off, declares none, waits until podman lists none of them, then stops the service. Removes any `50-mica-*.container` file in STATE's `quadlet` directory | the same, `mica-containerd` |
| `mqtt` | `/run/mica/mqtt-broker.toml`, `/run/mica/mqttd-device.env`, both MQTT units | the same, both scripts |
| `time` | A timesyncd drop-in, `/run/mica/timezone` | `ntp_servers` in `/run/mica/ntpd.conf`, `mica-ntpd` restarted |
| `web` | `access.web` into `/run/mica/apid.json`; a running `apid.service` restarted when it changed | the same, `apid` |
| `bluetooth` | `bluetooth.service`, the adapter's properties, each declared device's trust; a paired device nobody declares is removed | the same through `mica-bluetoothd` |

Services are driven through one trait (`UnitControl`): systemd over D-Bus, with runtime
enablement; on OpenRC, `rc-service` (`x.service` is `x`, `a@b.service` is `a.b`), with enablement
a no-op because `/etc/runlevels` is read-only and micad starts what it wants on every boot.

### 3.5 D-Bus

Bus name `com.mica.micad`, object `/com/mica/micad`, interface `com.mica.micad1`, values as JSON
strings. The policy (`crates/micad/dist/com.mica.micad.conf`) makes it root-only in both
directions, because `SettingsChanged` carries setting values.

| Group | Members |
| --- | --- |
| Settings and state | `GetSettings`, `SetSettings`, `GetTask`, `GetState`, `ReportHealth`, `ForgetService`; signals `SettingsChanged`, `TaskChanged` |
| Observation | `GetNetworkState`, `GetObservedNetwork`, `GetTimeStatus`, `GetStorageStatus`, `GetSystemInfo`, `GetTelemetry`, `GetFailureEvidence`, `GetLog`, `GetContainers`, `ScanWifi`, `GetBluetooth` |
| Actions | `Reboot`, `PowerOff`, `SetTransientRootPassword`, `RotateWireguardKey` |
| Updates | `GetUpdateState`, `CheckUpdate`, `FetchUpdate`, `ImportUpdate`, `InstallUpdate`, `ConfirmDeployment`, `RejectDeployment`, `RollbackDeployment`, `SetRebootOverride`, `SetUpdateConfig` |
| Containers and Bluetooth | `StartContainer`, `StopContainer`, `RestartContainer`, `SetBluetoothDiscovery`, `PairBluetoothDevice`, `ConfirmBluetoothPairing`, `RemoveBluetoothDevice` |

A value the tree refuses is `org.freedesktop.DBus.Error.InvalidArgs`.

### 3.6 Observation

Every observer is a trait with an "unavailable" default, so a test never reads its host, and
absence is reported with its reason rather than as a healthy reading:

- **Network**: networkd `Describe`, wpa_supplicant and hostapd sockets and a resolved probe; on
  OpenRC the same document from sysfs, busybox `ip` and `/run/mica/resolv.d`.
- **Time**: timesyncd; on OpenRC whether `mica-ntpd` runs and the kernel's synchronized bit.
- **Storage**: tiers from the signed boot policy, bind namespaces, usage, DATA's project quotas
  (`quotactl_fd`), media health and pressure.
- **System and telemetry**: identity, board, kernel, image, packages, the running deployment;
  thermal zones, watchdogs, the reset reason.
- **Failure evidence**: failed services and warnings of this boot (`journalctl`, or `logread`
  and `mica-init failed`).
- **Logs** (`GetLog`): the newest 200 lines of one allowlisted service (`micad`, `apid`, `time`,
  `ssh`, `wifi-client`, `wifi-ap`, `bluetooth`, `mqtt-broker`, `mqttd`), bounded in bytes and
  time.

### 3.7 Lifecycle

- **Provisioning.** First boot generates identity and per-device credentials on the device;
  nothing secret is baked into an image. A provisioning document on the boot medium or removable
  media is validated whole, applied atomically and recorded, so a second application is a no-op.
- **Updates.** micad runs `mica-deploy` with bounded time and output, and adds the operator's
  policy: the check cadence and its optional time of day, the maintenance window, the
  safe-to-reboot gate and its bounded override, and the failed IDs and generation floor that keep
  a rejected release from coming back.
- **Power, reset and recovery.** Reboot and power-off are recorded with their caller before they
  run. A staged reset applies one tier: `configuration`, `application-data` or `full-factory`
  (which keeps identity). A board-declared physical recovery action at boot maps to a tier. A
  transient root password lives in `/etc/shadow` until the next boot and in no setting.
- **Service registry.** Other `com.mica.*` names are recorded as they appear and vanish; the name
  rule is `crates/mica-busname`, shared with the MQTT bridge.

### 3.8 What micad drives on OpenRC, and who ships it

The contract between micad and the packages whose services it drives on an OpenRC root:

| Service | Shipped by | micad writes |
| --- | --- | --- |
| `mica-dropbear` | mica-ssh | `/run/mica/dropbear.env` (`DROPBEAR_ARGS`) |
| `mica-wifi-client` | mica-wifi | `/run/mica/wifi-client.env` (`interface`, `config`) |
| `mica-wifi-ap` | mica-wifi-ap | `/run/mica/wifi-ap.env` (`interface`, `config`, `address`, `udhcpd`), `/run/mica/wifi-ap-udhcpd.conf` (pidfile `/run/mica-wifi-ap.udhcpd.pid`) |
| `mica-bluetoothd` | mica-bluetooth | -- |
| `mica-ntpd` | mica-openrc | `/run/mica/ntpd.conf` (`ntp_servers`) |
| `mica-network` | mica-openrc | `/var/lib/mica/network/interfaces` |

Failed services are read with mica-openrc's `/usr/lib/mica/mica-init failed`, the log with
`logread`, and power goes through `openrc-shutdown`.

## 4. mica-apid

The same executable as micad under the name `mica-apid`: the API and the console. It holds
no configuration of its own; every read and change goes through micad.

It listens where `access.web` says (`GET`/`PUT /api/v1/web`, the console's access page), read at
start from `/run/mica/apid.json`: by default plain HTTP on 8080 and no HTTPS, so it takes neither
80 nor 443 from another service. With `httpsEnabled` it serves HTTPS on `httpsPort` (default 8443)
with its self-signed identity and the HTTP port only redirects there; the session cookie is
`Secure` exactly then. A port another enabled listener (SSH, MQTT) holds is refused. A change
restarts apid on the new listeners; `mica-apid --healthcheck` probes the same listener.

The HTTPS identity is `identity.pem` in the state directory, the chain and its key in one 0600
file, generated self-signed (`mica`, `localhost`, `127.0.0.1`) the first time HTTPS serves.
`/api/v1/web/certificate` describes it (never the key) and replaces it with an uploaded chain and
key, which must match and be inside their validity; `.../certificate/generate` replaces it with a
fresh self-signed one for the names and days given. Either takes effect on the next connection,
with no restart. A configuration or full-factory reset removes it.

| Variable | Default | Meaning |
| --- | --- | --- |
| `APID_LISTENERS_FILE` | `/run/mica/apid.json` | The rendered `access.web` |
| `APID_HTTPS_ADDR` | unset | Overrides the file: HTTPS on this address |
| `APID_HTTP_ADDR` | unset | Overrides the file: HTTP on this address |
| `APID_STATE_DIR` | `/var/lib/mica/apid` | TLS identity, session key, login backoff, audit ring |
| `APID_BUS` | `system` | The bus micad is on |

`--version`, `--openapi` (the committed `crates/mica-apid/openapi.json`, which the gate holds to
the handlers) and `--healthcheck` (for the boot health gate) answer without starting a daemon.

**URL space.** `/api/v1/...` is the API; `/api/versions` lists the versions served; `/healthz` is
liveness; `/_ui/` is the built-in console from `mica-apid-ui` (404 without the package); `/` is
the active custom console bundle or a redirect to `/_ui/`.

**The API**, by group: session, setup and password; API tokens; settings, state and tasks;
network, Wi-Fi, containers, Bluetooth, MQTT and SSH keys; observation (time, storage, system
information, telemetry, service logs at `/system/logs/{source}`, network status, health, meta);
actions (reboot, power off, transient root password, WireGuard key rotation); updates (check,
fetch, import of an uploaded `.micaupd`, install, confirm, reject, rollback, reboot override,
policy); provisioning status and claim; credential recovery and reset; diagnostic snapshots;
console bundles. `GET /meta` lists the product's features, and the console hides what is absent.

**Authentication.** Until an admin password exists, `POST /setup` sets it. Browsers exchange the
password for an HMAC-signed, HttpOnly session cookie and send the session's CSRF token on every
mutation; automation uses bearer tokens `mica_<id>_<secret>`, stored as digests. Passwords are
argon2id, failed logins back off persistently, and security-relevant actions go to an audit ring.

**What leaves the device.** Settings and state pass a structural redactor, so secrets such as
password hashes are never served. A diagnostic snapshot is bounded, redacted and stored in
`/mica/diagnostics`; the lines of a service log are scrubbed the same way (a line carrying a
secret marker is replaced whole, a hardware address masked).

**Consoles.** The built-in console is React and Vite (`crates/mica-apid/ui`), built with Bun in
the base image and shipped inside the verified root. An operator can upload a `.mica-ui.zip`
bundle to `/mica/ui`; it is validated (`crates/mica-ui-bundle`), installed as a generation and
activated explicitly, and re-checked against the API at every start.

**Reaching a shell.** Claim the device, then either add a public key to `access.ssh` (SSH is off
until enabled; keys apply to `root` and `mica`) or set a transient root password (8 to 72 bytes,
gone at the next boot). dropbear authenticates against `/etc/shadow` through `crypt(3)`, without
PAM; mica-system-base asserts that of the package it pins.

## 5. MQTT

MQTT carries application data only; nothing of configuration, state, credentials or updates.

- **mica-mqtt-broker** is rumqttd used as a library: one MQTT 3.1.1 listener built in code from
  the three keys micad renders (address, port, whether clients authenticate; accounts in
  `/var/lib/mica/mqtt-broker-users.toml`). No console, metrics, clustering or bridging exist to
  turn on. It runs as its own user.
- **mica-mqttd** bridges only enrolled applications: each file name in
  `/usr/lib/mica/mqtt-applications.d/` is a D-Bus name `com.mica.<class>[.<suffix>]`, whose
  package grants the bridge access to `GetItems`, `ItemsChanged` and `SetValue`. It has no access
  to micad. Topics are `N/<deviceId>/<class>/<instance>/<path>` (published), `R/...` (read) and
  `W/...` (write, only in `full` mode; `read-only` is the default). The protocol is a pure state
  machine (`bridge.rs`) between a transport and the application buses.

## 6. Deployments and boot

### 6.1 Storage

| Where | Holds |
| --- | --- |
| SYSTEM (`/mnt/system`) | At most two deployments: descriptors, signatures, content-addressed objects under `kernels/`, `roots/` and `cores/<id>/core.{img,roothash.p7s,roothash}` |
| ESP, or the U-Boot environment on a FIT board | The boot records that select a deployment and count trial attempts |
| DATA `/mnt/data/meta` | Transaction state, catalog checkpoints, the firmware receipt |
| DATA `/mica/updates` | Downloads and staged objects; an acquisition never writes SYSTEM |
| DATA project directories | Quotas set by the runkit: `mica` and `srv` project 100, `cache`, `tmp` and `var` project 101 (one eighth of DATA, 32 to 256 MiB, 2048 to 16384 inodes), `containers` project 102 |

SYSTEM and the boot partition are mounted read-only and made writable only inside an operation,
under the transaction lock.

### 6.2 Signed metadata

Strict JSON verified with Ed25519 against keys embedded in the signed kernel; unknown fields are
refused and field order is part of the wire form. `mica/deployment/v1` (a deployment, its product
and components), `mica/kernel/v1` and `mica/rootfs/v1` (components and their verity parameters; a
root states its `interfaceLevel`, 1 or more), `mica/update-envelope/v1` and `mica/firmware/v1`
(firmware, never carried by a deployment). A deployment's optional `core` lists its `mica/core/v1`
components: one per package, sorted, each on the deployment's architecture, running on its root's
level, with every `needs` met inside the deployment. Install, collection and acquisition handle a
core component's objects like the root's, and the runkit composes them (6.5).
Offline archives start with `MICAUPD1` and may carry any subset of a descriptor's objects.

The server catalog is **unsigned** and comes in three documents, each fetched only when needed.
The configured source is an **update root** ending in `/` (http or https, a host, no user info,
query or fragment, such as `https://res.micaos.dev/update/`). The reader appends the manifest
major it reads, `v2/manifest.json`:

- **The manifest**, `mica/catalog/v2`: one line per board and product, naming the current
  release's id, generation, notes and `path`. A device that is current, or whose product has no
  line, reads nothing more.
- **The release's document**, `mica/release/v1`: it must agree with its line (id, board, product,
  generation). It names the descriptor (`path`, `sha256`, `bytes`) and each object (`sha256`,
  `bytes`, `path`).
- **The signed descriptor**: it must match that digest and length, authenticate, and name the
  same board, product and generation as the line, the device's arch, and exactly those objects.

Each document's links are `baseUrl` + `path`. A `baseUrl` ends in `/`, has a host and no user
info, query or fragment, and is https unless the source is http. A `path` stays below its base.
Both unsigned documents are canonical JSON (sorted keys, no whitespace). Trust comes from the
signed descriptor, not from an address: a device takes only its own board, arch and product, only
a generation above the one it runs, and checks every byte against what the descriptor names. The
manifest revision is checkpointed as a consistency check against a confused mirror, not as a
security control; there is no expiry, so a withheld catalog reads as "nothing newer".

**Compatibility.** The version is in the path, chosen by the reader, so a device's configuration
never names a format. Within a major, documents only grow: the reader ignores fields it does not
know. A change an older reader cannot ignore gets a new major at a new path (`v3/manifest.json`),
and the server keeps writing the old major for as long as devices read it. The old major's
current release must be a stepping stone whose reader takes both majors. The signed descriptor
refuses unknown fields, so a change to it follows the same rule: first ship a reader that takes
the new schema, then publish it.

The operator
document `/mica/config/updates.json` is `mica/update-config/v1` and the baked defaults are
`mica/meta/v1` (`update` is `source`, `policy`, `checkIntervalMinutes`).

The shared fixtures in `crates/mica-deploy/tests/component-contracts/` (the documents, the
catalog, the refused schemas and cases, the chunker vector and the broken indexes) are generated
by the `component-contract-fixtures` example, signed with test-only keys, and copied by mica-build.

### 6.3 mica-deploy

`/usr/bin/mica-deploy <command>` prints JSON, exits non-zero on failure and bounds every read by
the caller's budget (`--max-bytes`): `status`, `check`, `fetch`, `import`, `install`, `booted`,
`confirm`, `reject`, `rollback`, `fail-boot`, `probe`, `discard`, `gc`, `firmware-readback`.
Installation is a durable file transaction, proven by a fault suite that interrupts it before
and after every observed IO.

**Delta transfer.** `fetch` first assembles a missing object from chunks the device already holds
(a content-defined cut both sides perform: `GEAR[b] = sha256("mica-chunker/v1" || b)[0..8]`, cut
when the top 14 bits of the rolling hash are zero after 4096 bytes, or at 65536), fetching the
rest from `chunks/<digest>` beside the object's directory (`<store>/objects/<sha256>` and
`<store>/chunks/<digest>`) by the object's `<path>.index` (`MICAIDX1`). Nothing in that
path is trusted: the result is verified like a whole download, and any failure falls back to the
whole object. An origin that publishes no index is still conforming.

**Confirm** means the slot can still be reached and changed: the boot health gate (mica-system's
`mica-health`) checks that the boot settled, micad answers on the bus and apid answers
`/healthz`, then confirms. Failed units are reported, never fatal.

### 6.4 The board, from the signed policy

Nothing here knows a board by name. The `board` section of the signed boot policy
(`/etc/mica/boot.json` in the initramfs, `/run/mica/boot-policy.json` after boot) states it:
`boot` (`uefi` or `uboot-fit`), `kernel` (`uki` or `fit`), `partitions` (the GPT numbers of the
boot partition, SYSTEM and DATA), `firmware` (the target a firmware receipt carries: `efi`,
`disk-range` or an eMMC boot area), `records` (the FIT record geometry), and optionally
`watchdog`, `reserves` (free space kept on SYSTEM, the ESP and DATA, 1 MiB to 1 GiB) and
`dataQuotas` (`false` mounts DATA without project quotas). The section is validated before use,
and the disk is held to the stated geometry before anything is written.

### 6.5 mica-runkit and lifecycle-sys

`mica-runkit` is one static executable selected by the name it runs as:

- **`init`**, PID 1 of the signed initramfs: requires dm-verity signature enforcement, arms the
  watchdog, reads the policy, authenticates the selected deployment, opens root and support as
  signed dm-verity mappings, composes the core components (below), mounts DATA (with `prjquota` and the project directories unless the
  board turns quotas off), seeds the persistent machine identity, binds modules and firmware, and
  hands over to the root's init. No network and no shell exist at this stage.
- **`shutdown`**, PID 1 of a memory-only exit ramdisk that systemd execs at the end of its
  shutdown: releases loops and device-mapper mappings in dependency order within a bounded budget,
  and releases storage only once nothing is mounted, mapped or looped and no page is left to
  write (the dirty count read after folding every CPU's vm statistics, `stat_refresh`). A boot
  the runkit refuses takes the same path.
  An OpenRC root gets no exit ramdisk: `openrc-init` reboots by itself once the shutdown runlevel
  has left DATA read-only.

**Core components at boot.** Each core component of the deployment is opened like the root, a
signed dm-verity mapping `mica-core-<package>` mounted at `/run/mica-core/<package>`, before
anything binds into the root's trees. The runkit then refuses a composition that would shadow a
file (a component path that is not a directory where the root or another component has it, or
anything outside `/usr` and `/etc`), and mounts `/usr` and `/etc` each as a read-only overlay of
the components' trees, in package order, over the root's own, with no upper layer. The files
appear at the paths a package would have used (`/usr/bin/micad`, `/etc/init.d/apid`, the units
and their enablement links). Shutdown releases the component mappings with the root's.

**The root interface level.** A `mica/rootfs/v1` root states an `interfaceLevel`, and each core
component the levels it runs on (`root: { min, max }`); a deployment whose components do not run
on its own root is refused when it is read. The level moves only when something a core component
relies on from the root changes, so an ordinary root update keeps it. Level 1 is:

- glibc and the libraries `micad` links against (`ldd /usr/bin/micad`), from the Base release;
- the init and its service contract: systemd with `multi-user.target`, or OpenRC with
  `supervise-daemon` and the `default` runlevel, and the services of section 3.8 by name;
- `mica-system`, the D-Bus system bus and `/usr/share/dbus-1/system.d`;
- the root's mount points and runtime directories (`/run/mica`, `/var/lib/mica`, `/mica`);
- the packages built into the root from this repository, at interfaces micad calls:
  `mica-deploy` (its command line and JSON), `mica-mqttd` and `mica-mqtt-broker` (their units and
  `/run/mica` files), `mica-sftp-server`.

mica-build raises the level whenever one of these changes incompatibly, and a core component
raises its `root.min` when it needs the change.

`crates/lifecycle-sys` is the only crate allowed `unsafe`: typed, bounded wrappers for the
watchdog, loop devices, device-mapper, ext4 project ids and quotas (`FS_IOC_FSSETXATTR`,
`quotactl_fd`) and the clock's synchronized bit. The boot/shutdown gate proves every structure
and request number against the kernel headers for x86-64 and aarch64
(`scripts/gate/boot-shutdown/uapi.c`).

## 7. Building, checking and releasing

The host needs docker with buildx, bash, git, make, curl and jq: every compile, pack and gate runs
in the images of `locks/mica-build-env.lock`. `make help` lists the targets.

```sh
make deps                  # every lock and pin, verified against its release
make check                 # lint, UI contract, Rust gate, boot/shutdown, IO faults
make deb MICA_ARCH=amd64   # the archives and core components, in _out/debs/<arch>/pool
make package-gate          # the package rules over both pools
```

### 7.1 Conventions

- Rust edition 2024, `rust-version` equal to the locked rustc (1.99; the gate holds them equal).
  `[workspace.lints]` makes every warning an error, forbids `unsafe` everywhere except
  `lifecycle-sys`, and denies `unwrap`, `expect` and `panic!` outside tests; an invariant the
  types cannot state is `expect("INVARIANT: ...")` with a local allow.
- No source file over about 600 lines: a module splits by responsibility into `foo/`, its tests
  into `foo/tests.rs`; an integration test that splits becomes `tests/<name>/main.rs`.
- One implementation per job: `mica-fs` for durable files (atomic replace, directory sync,
  bounded reads), `micad-settings` for the rules apid and micad share, aws-lc-rs as the only
  crypto library (rustls, digests, HMAC, random, Ed25519, X25519).
- `Cargo.lock` is committed and every build is `--locked`. Release builds are stripped,
  whole-program LTO in one codegen unit, optimized for size.
- Shell is bash with `set -euo pipefail`, and never an early-exiting reader on the right of a
  pipe.

### 7.2 Gates

| Gate | Checks |
| --- | --- |
| `make lint` | The shell rule above |
| `make apid-ui-build-contract-test` | The console builds as production assets; its lint, types and tests |
| `make rust-gate` | Each producer's upstream version is its binaries' crate version and `mica-inputs` names their cargo closure; `rust-version` is the locked rustc; fmt, clippy, nextest, doctests, cargo-deny, cargo-shear, typos; the committed OpenAPI document is what `mica-apid --openapi` prints |
| `make boot-shutdown-test` | The boot and shutdown suites and the UAPI proof (`--arm-abi` adds aarch64) |
| `make file-transaction-faults` | mica-deploy's transactions interrupted at every IO |
| `make dbus-policy-test` | The micad bus policy against a real bus (root, host `dbus-daemon`) |
| `make package-gate` | Pool rules (names, architecture, no commit stamp, no path in two packages, copyright, no conffiles, POSIX maintainer scripts); the declared packages and core components at their versions and no other item, each core record an unsigned `mica/core/v1` naming its own image, exact dependency versions, enablement links, nothing outside `/usr` and `/etc` in a component, component hash trees that verify, and a byte-identical rebuild (of the core components on amd64) |

### 7.3 Producers and versions

A producer is a directory of `pkgs/`: `mica-inputs` (its packages and the tracked paths that
decide their bytes: the crates of its binaries, the workspace manifest and lock, the packing
scripts, less tests and examples), `producer.env` (`ENABLEMENT`, the enablement links each
package ships; `CONTEXTS`, extra build contexts such as `crates/*/dist`), `prepare.sh`, a
`Dockerfile` and, per package, one `control/<package>.control` for a Debian package or one
`core/<package>.json` for a core component (package, version, `sourceDateEpoch`, features, needs,
root interface range); `pkgs/copyright` is shared.

| Producer | Binaries | Packages |
| --- | --- | --- |
| `micad` | `micad` | core components `micad`, `mica-apid-ui` |
| `mqtt` | `mica-mqttd`, `mica-mqtt-broker` | `mica-mqttd`, `mica-mqtt-broker` |
| `sftp` | `mica-sftp-server` | `mica-sftp-server` |
| `deploy` | `mica-deploy` | `mica-deploy` |
| `lifecycle` | `mica-runkit` (static) | `mica-lifecycle` |

`scripts/deb/build.sh` compiles a producer's binaries in the rust image of the host architecture
(native on an arm64 runner, cross-compiled otherwise), into a target directory of its own, and
packs them with `mica-tools deb pack` in the base image of the target architecture, so
`${shlibs:Depends}` is resolved against the target's libraries. A core component is staged and
packed by `scripts/deb/core-image.sh` in the pinned alpine image on the build platform: squashfs
(zstd, one processor, root ownership, no xattrs) and its dm-verity hash tree, salted with the
squashfs's own sha256, and the canonical record. Every mtime is `SOURCE_DATE_EPOCH` and no field
names a commit.

A Debian package's version is `<major>.<minor>.<patch>-<revision>` and a core component's
`<major>.<minor>.<patch>`; the upstream part is the crate version of its binaries, and the whole
version is printed by `--version`. A source change bumps the upstream part and resets the
revision to 1; a packaging-only change bumps the revision. Because the workspace manifest and lock
are every producer's input, a crate version bump moves every producer. `mica-tools pool guard` compares each archive with the newest release: at a released
version the inputs and bytes must be identical, a higher version is new, a lower one is refused.
A locally cross-built arm64 archive differs from the native one a release carries, so the arm64
half is the CI's to judge.

### 7.4 CI and releases

`ci.yml` runs on every push to `main` and every pull request: `make deps` and `make check`, then
a native amd64 and arm64 build, pack and package gate, then both pools gated together with the
guard. It publishes nothing.

A release is named by its UTC minute, `YYYYMMDD-HHMM`, and cut on a commit of `main` with
`gh release create <YYYYMMDD-HHMM> --target <commit>`. `release.yml` builds and gates the tag,
then `scripts/build/release.sh` pushes the pools as `ghcr.io/micaoss/mica-core:pool.<arch>.<tag>`
(one layer per archive and two per core component, its image and its record, each annotated
with its producer's inputs hash; a component's layers are `application/vnd.mica.item.core.img`
and `application/vnd.mica.item.core.json`) and attaches:

| Asset | Content |
| --- | --- |
| `mica-core.lock` | `mica-lock v1`: the `release` row, a `pool` row per architecture by digest, a `package` row per package and architecture (version and sha256), and per core component and architecture an `item core.img` row (version and the image's sha256) and an `item core.json` row (version and the record's sha256) |
| `SHA256SUMS` | The sha256 of `mica-core.lock` |

A consumer commits `mica-core.lock` unchanged with a pin naming the release and the sha256 of its
`SHA256SUMS`, and takes each archive from its pool by the package row's digest and each core
component's record and image by their item rows' digests; the record names the same image digest.

### 7.5 Moving an input

Replace `locks/<repository>.lock` with the release's lock and `locks/pins/<repository>.pin` with
the release and the sha256 of its `SHA256SUMS`, together and nothing else; `make deps` must pass.
`bin/mica-tools` is a copy of the pinned mica-build-tools bootstrap, which checks the pinned
commit out into the ignored `repos/`.

### 7.6 Running the daemons locally

Never against the host's bus or files. Start a private `dbus-daemon --session`, and run micad
with `MICAD_BUS=session MICAD_DRY_RUN=1` and settings and config paths in a temporary directory;
apid joins with `APID_BUS=session` and its own listeners and state directory. The e2e suite
(`crates/mica-apid/tests/e2e`) does exactly this. The console develops with `bun run dev:bare`
under `crates/mica-apid/ui`; the shipped build is always `crates/mica-apid/ui/build.sh` in the
base image.
