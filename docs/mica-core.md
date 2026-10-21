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
| `micaoss/mica-pkgs` | The feature packages: the MQTT broker and bridge, the SFTP server | micad renders the MQTT configuration and starts both MQTT services by name (`mica-pkgs:docs/mica-pkgs.md` section 3) |
| `micaoss/mica-podman` | The container engine and its supervisor, mica-containerd | micad's container reconciler starts mica-containerd and declares the containers to it |
| `micaoss/mica-build` | Boards, products and the signed images | Pins a mica-core release, installs its packages into each product's root, and signs its core components into core sets, one of which every image carries |
| `micaoss/mica-res` | The resource service behind `res.micaos.dev` | Serves the update root mica-deploy reads (6.2) |
| `micaoss/mica-fleet` | The fleet service | None yet: apid reports the fleet document (6.2), and nothing on the device is a client of it |
| `micaoss/mica` | The user documentation | Describes what an operator sees of micad, apid and the console |

## 2. Core components and packages

Two things ship as **core components**, not as Debian packages: they are composed over the root
at boot from the device's core set (6.2), so a mica-core release reaches devices without a new
root or a new deployment. Each is a squashfs of its `/usr` and `/etc`
with an appended dm-verity hash tree, and an unsigned `mica/core/v1` record naming it; both are
layers of the release's pool, pool items of the types `core.img` and `core.json` named by `item`
rows of `mica-core.lock` (`mica-build-tools:docs/spec/release-lock.md` 1.2.7). mica-build signs the
root hash with the product's key.

| Component | What it holds | systemd | OpenRC | Started |
| --- | --- | --- | --- | --- |
| `micad` | The management daemon and its D-Bus policy, and the API, `/usr/bin/mica-apid` (a link to `micad`, the same as `micad apid`), with the OpenAPI document; the update command `/usr/bin/mica-update` (a link, the same as `micad update`) and the containers command `/usr/bin/mica-ctr` (a link, the same as `micad ctr`); feature `micad` | `micad.service`, `apid.service` | `/etc/init.d/micad`, `/etc/init.d/apid` | At boot (the component links both) |
| `mica-apid-ui` | The web console at `/usr/share/mica-apid/ui`; feature `ui`, at micad's exact version | -- | -- | Served by apid; optional |

The rest are Debian packages built into the root:

| Package | What it installs | systemd | OpenRC | Started |
| --- | --- | --- | --- | --- |
| `mica-deploy` | The deployment client | -- | -- | By micad and the boot health gate |
| `mica-lifecycle` | `mica-runkit`, static, which the assembly packs into the signed kernel | -- | -- | PID 1 of the initramfs and the exit ramdisk |

**Each component and package carries the start-up files of its own services, for both inits, and
none of another's.** micad and apid enable themselves: a `multi-user.target.wants` link and an
`/etc/runlevels/default` link each, in the micad component (mica-openrc keeps runlevel links it
finds). The MQTT daemons of mica-pkgs are enabled on neither init;
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
credentials; the product file is read; the reconcilers are built; the reconcilers that only
render files and make bus calls (hostname, network, web) are applied once; the bus name is
claimed; then the ones that start or restart another service (sshd, wifiClient, wifiAp,
container, mqtt, time, bluetooth) are applied, again every 10 s while one fails, for up to five
minutes. micad's readiness thus waits on no other service: the container reconciler waits for
mica-containerd, and on OpenRC `rc-service start` waits for the service and what it needs (the
MQTT units need micad itself, mica-ntpd the network).

| Variable | Default | Meaning |
| --- | --- | --- |
| `MICAD_SETTINGS_PATH` | `/var/lib/mica/settings.toml` | The STATE document |
| `MICAD_CONFIG_DIR` | `/mica/config` | The configuration documents on DATA |
| `MICAD_BUS` | `system` | `system` or `session` |
| `MICAD_SHADOW_PATH` | `/etc/shadow` | Where a transient root password goes |
| `MICAD_META_MANIFEST_PATH` | `/usr/share/mica/meta/updates/manifest.json` | The baked update configuration |
| `MICAD_UPDATE_POLICY_PATH` | `/mica/config/updates.json` | The operator's update policy |
| `MICAD_PROVISIONING_ROOT` | `/run/mica/provisioning` | Where offline provisioning media are staged |
| `MICAD_DEFAULTS_DIR` | `/usr/share/mica/defaults` | The board's default settings (3.3) |
| `MICAD_DRY_RUN` | unset | `1`: no provisioning, no reconcilers, no host access (the tests) |

The tests relocate the rest of what micad writes the same way: the reconcilers' output
(`MICAD_NETWORK_DIR`, `MICAD_WPA_SUPPLICANT_DIR`, `MICAD_HOSTAPD_DIR`, `MICAD_DROPBEAR_ENV`,
`MICAD_PASSWD_PATH`, `MICAD_MQTT_BROKER_CONFIG`), the DATA pool a reset clears (`MICA_DATA_ROOT`,
`/mnt/data`), the audit ring (`MICA_AUDIT_RING_DIR`) and the recovery inputs
(`MICA_RECOVERY_DECLARATION_PATH`, `MICA_RECOVERY_CMDLINE_PATH`, `MICA_PRESENCE_MARKER_PATH`).

### 3.2 The product

`/usr/lib/mica/product.conf`, written when the root is composed and authenticated with it:

- `PRODUCT` names the product a deployment must be built for.
- `FEATURES` lists what the product carries. micad serves `wifi`, `bluetooth`, `ssh`,
  `containers` and `mqtt`: the reconciler, the bus members and the API routes of one not listed
  do not exist on the device; a write to its subtree is refused as `feature not in this product`,
  and its routes answer 404. Other words are ignored by micad, and no file, or no `FEATURES` line,
  serves every feature. The same words select the core components at boot (6.2): `micad` the
  daemon, `ui` the console. The runkit reads the line exactly once and quoted, and cannot compose
  the core set of a root without it.
- `INIT` is `systemd` (the default) or `openrc`, and decides every backend below. An unknown
  init stops micad.
- `CONTAINER_ENGINE` is `podman` (the default) or `docker`, and decides the container backend
  (3.9). An unknown engine stops micad.

### 3.3 Settings

One typed tree (`crates/micad-settings`), addressed by dot-paths:

| Subtree | Contents |
| --- | --- |
| `hostname` | The hostname |
| `network.<iface>` | `kind` (`physical`, `vlan`, `bridge`, `wireguard`, `can`) and its block, DHCP (optionally with `dns`, servers used instead of the lease's; `dhcp` absent is false) or static addressing, routes, a DHCP server; a `can` entry has its block alone (below) |
| `dns` | `global` (`enabled` and up to three IPv4 `servers`, kept while switched off) replaces every interface's servers; `staticRecords` (up to 100 `{hostname, address, comment?}`) go into the hosts file |
| `access` | SSH (switch, listen addresses, authorized keys), the console, the API's listeners (`web`), the admin password hash, the device credential, API tokens, claim state |
| `wifi` | `client` (the station, on by default, and its known networks) and `ap` (the access point, off by default) |
| `container` | The engine switch (`enabled`) and the declared Compose projects (`projects`, section 3.9) |
| `bluetooth` | The switch (on by default), discoverability (off by default), the name, the pairing code (six digits; `000000` until the device is claimed with Bluetooth on, then six random digits unless one is declared; a write keeps the stored code when it carries none, and apid refuses `000000`, which only a reset returns to), the trust list |
| `mqtt` | The switch, the broker listener (default `127.0.0.1:1883`), whether clients authenticate |
| `time` | NTP servers and the presentation timezone |
| `services` | The user's own services (`units`, section 3.10) |
| `provisioning`, `reset` | Provisioning status; a staged reset |

What an integrator sets lives on DATA, one JSON document per concern in `/mica/config/`
(`system`, `network`, `dns`, `wifi`, `ssh`, `mqtt`, `time`, `container`, `bluetooth`, `services`, directory `0700`,
documents `0600`), beside `updates.json`; what the device mints (identity, credentials,
provisioning state, a staged reset) is `settings.toml` on STATE. Each document has its own
schema version, and an unknown key or version is refused, never converted. A document that does
not parse disables only the subtrees it carries. Writes are validated against the whole tree,
go through a fsynced undo journal, and are applied as a task (`SetSettings` returns its id;
`TaskChanged` reports it).

**CAN interfaces.** `kind: "can"` is a CAN controller the kernel registered (`can0`); its
`can` block:

| Field | Meaning |
| --- | --- |
| `bitrate` | Required: the nominal (arbitration) bit rate, 1 to 1 000 000 bit/s |
| `restartMs` | The delay before the controller restarts after bus-off, 0 to 60 000 ms; 0 or absent never restarts it |
| `fd` | `true` switches CAN FD on; `false` and absent switch it off, which an FD-capable controller needs to come up at a classic bit rate and a controller without CAN FD accepts |
| `dataBitrate` | The CAN FD data-phase bit rate, 1 to 8 000 000 bit/s; required with `fd: true`, refused otherwise |

A CAN entry carries no addressing: `dhcp` true, `static`, `dns`, `routes` and `dhcpServer` are
refused, and so is a CAN entry named as a bridge port or a VLAN parent. A `can` block on an entry
of another kind is refused.

**Board defaults.** A board package may ship `/usr/share/mica/defaults/network.json`, a
`network.json` document of this schema (`schema_version` and `network`). Every boot, after
first-boot provisioning (3.7), micad takes each entry it declares that STATE's record
`seeded-defaults.json` does not name: one the tree does not declare is added and saved to DATA,
and either way the name is recorded. So a provisioning document's entry is the tree's, a device
that updates onto a board with a new default gets it, an added entry is edited like any other,
and one the operator deleted stays deleted. A `configuration` or `full-factory` reset removes the
record, and the defaults come back. A defaults document that does not parse or validate is
logged and adds nothing. cx3576 ships `can0` with `bitrate` 250 000, `restartMs` 100 and `fd`
false.

### 3.4 Reconcilers

Each owns one subtree and converges the system to it, returning the applied state into the
live-state tree under its name.

| Reconciler | systemd | OpenRC |
| --- | --- | --- |
| `hostname` | `/etc/hostname`, hostnamed | `/etc/hostname`, `sethostname(2)` |
| `network` | networkd `.network`/`.netdev` under `/run/systemd/network` for every kind; WireGuard keys generated on the device and named by file; a `can` entry is a `.network` of `[Match]` and `[CAN]` alone, with `BitRate=`, `RestartSec=<restartMs>ms` and `DataBitRate=` for the fields it declares and `FDMode=yes` or, otherwise, `FDMode=no` | `/var/lib/mica/network/interfaces` for busybox ifupdown (`ifdown -a` on the old file, `ifup -a` on the new): DHCP (with `dns`, the lease is asked for no DNS option and the servers go into `/run/mica/resolv.d/00-<iface>`), static IPv4/IPv6 with DNS into `/run/mica/resolv.d/<iface>`, or no addressing; undeclared `eth*` get DHCP. A `can` entry is an `inet manual` stanza that sets the link with iproute2's `ip`, by its path (the first of `/usr/sbin/ip`, `/usr/bin/ip`, `/sbin/ip`, `/bin/ip`): `pre-up <ip> link set dev <iface> type can bitrate <b>` with `restart-ms <ms>` when declared and `fd on dbitrate <d>` or, otherwise, `fd off`, then `<ip> link set dev <iface> up` (and `down`); a root whose `ip` is absent or BusyBox's (which cannot set CAN parameters) refuses a CAN entry by name and writes nothing. VLAN, bridge, WireGuard, routes and the DHCP server are refused by name |
| `dns` | `/run/mica/hosts` (where `/etc/hosts` links): localhost, `127.0.1.1` for the hostname and the static records, rewritten when the hostname changes. Global servers: resolved's `/run/systemd/resolved.conf.d/60-mica-dns.conf` (resolved restarted), and beside every `.network` a drop-in that clears its `DNS=` and sets `UseDNS=no` for DHCPv4, DHCPv6 and router advertisements (networkd reloaded) | the same hosts file. Global servers: `/run/mica/resolv.d/global`, which Base's `mica-resolv` makes the whole of `resolv.conf`, then `mica-resolv` |
| `sshd` | `/run/mica/dropbear.env`, the authorized keys of `root` and `mica`, `dropbear.service` | the same files, `mica-dropbear` |
| `wifi_client` | wpa_supplicant configuration in `/etc/wpa_supplicant`, `wpa_supplicant@<if>`, networkd DHCP | configuration in `/var/lib/mica/wpa_supplicant`, `/run/mica/wifi-client.env`, `mica-wifi-client` |
| `wifi_ap` | hostapd configuration in `/etc/hostapd`, `hostapd@<if>`, networkd address and DHCP server | configuration in `/var/lib/mica/hostapd`, `/run/mica/wifi-ap.env`, busybox udhcpd on the same pool (`/run/mica/wifi-ap-udhcpd.conf`), `mica-wifi-ap` |
| `container` | Starts `mica-containerd.service` and declares the projects as one set; switched off, stops every container, then stops the service (section 3.9) | the same, `mica-containerd` |
| `mqtt` | `/run/mica/mqtt-broker.toml`, `/run/mica/mqttd-device.env`, `mica-mqtt-broker.service` and `mica-mqttd.service` (`mica-pkgs:docs/mica-pkgs.md` section 3) | the same files, `mica-mqtt-broker` and `mica-mqttd` |
| `time` | A timesyncd drop-in, `/run/mica/timezone` | `ntp_servers` in `/run/mica/ntpd.conf`, `mica-ntpd` restarted |
| `web` | `access.web` into `/run/mica/apid.json`; a running `apid.service` restarted when it changed | the same, `apid` |
| `services` | The user's units through `mica-unit`, in `/usr/local/lib/systemd/system` (section 3.10) | the same, scripts in `/usr/local/etc` |
| `bluetooth` | `bluetooth.service`, the adapter's properties, each declared device's trust; a paired device nobody declares is removed. micad registers its pairing agent each time `org.bluez` appears on the bus. When the root carries `/usr/bin/mica-bled` (mica-pkgs), `/run/mica/bled.env` (the device id, apid's local socket) and `mica-bled.service` while Bluetooth is on and the adapter is there; otherwise the service is stopped and the file removed | the same through `mica-bluetoothd`, and `mica-bled` |

The station and the access point share one radio and take turns. The station is on by default:
its supplicant runs whenever `wifi.client.enabled` is set, with no network block until one is
added, so a scan works before any network is chosen and nothing is joined; networkd DHCP is
rendered only once a network exists. A device provisioned before the default changed keeps the
`enabled` it stored. An access point on the station's interface reports
`conflict` and does not start; the console asks the operator before switching the other role off,
and a scan without a station answers `reason` `access-point` or `client-off`.

Services are driven through one trait (`UnitControl`): systemd over D-Bus, with runtime
enablement; on OpenRC, `rc-service` (`x.service` is `x`, `a@b.service` is `a.b`), with enablement
a no-op because `/etc/runlevels` is read-only and micad starts what it wants on every boot.

### 3.5 D-Bus

Bus name `com.mica.micad`, object `/com/mica/micad`, interface `com.mica.micad1`, values as JSON
strings. The policy (`crates/micad/dist/com.mica.micad.conf`) makes it root-only in both
directions, because `SettingsChanged` carries setting values.

| Group | Members |
| --- | --- |
| Settings and state | `GetSettings`, `SetSettings`, `SetSettingsMany`, `GetTask`, `GetState`, `ReportHealth`, `ForgetService`; signals `SettingsChanged`, `TaskChanged` |
| Observation | `GetNetworkState`, `GetObservedNetwork`, `GetTimeStatus`, `GetStorageStatus`, `GetSystemInfo`, `GetTelemetry`, `GetFailureEvidence`, `GetLog`, `GetContainers`, `GetContainerLog`, `ScanWifi`, `TestDnsServers`, `GetBluetooth` |
| Actions | `Reboot`, `PowerOff`, `SetTransientRootPassword`, `RotateWireguardKey` |
| Updates | `GetUpdateState`, `CheckUpdate`, `FetchUpdate`, `ImportUpdate`, `InstallUpdate`, `InstallLatestUpdate`, `ConfirmDeployment`, `RejectDeployment`, `RollbackDeployment`, `SetRebootOverride`, `SetUpdateConfig` |
| The user's services | `GetUnits`, `StartUnit`, `StopUnit`, `RestartUnit`, `GetUnitLog` |
| Containers and Bluetooth | `CheckContainerProject`, `CheckContainerProjectIn`, `StartContainer`, `StopContainer`, `RestartContainer`, `SetBluetoothDiscovery`, `PairBluetoothDevice`, `ConfirmBluetoothPairing`, `RemoveBluetoothDevice` |

A value the tree refuses is `org.freedesktop.DBus.Error.InvalidArgs`.

### 3.6 Observation

Every observer is a trait with an "unavailable" default, so a test never reads its host, and
absence is reported with its reason rather than as a healthy reading:

- **Network**: networkd `Describe`, wpa_supplicant and hostapd sockets (micad's reply sockets are
  bound under `/run/mica`: its unit has a private `/tmp`, which neither daemon can answer into)
  and a resolved probe; on
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
  One update at a time, a set or a deployment: a check reads the core line of the channel the
  device follows (`channels.core`: the operator's, else the product's, else `general`) and names
  a newer core set that selects on the running root before it reads the product line. A set that
  does not select is shown as `update.lifecycle.waitsForSystem`, and a core line that cannot be
  read (a set-check that fails) as `update.lifecycle.coreCheckFailed` (`code`, `reason`); either
  way the check goes on to the product line, so a broken core line never blocks a system update. A system update on the `general` channel fetches the channel's latest set for the
  new root too (`set-fetch --deployment`), and `install` is given the newest staged `general` set
  (`--core-set`) and takes the set the table of 6.2 names, as one pair with the deployment.
  Fetch, install, the maintenance window and the reboot gate are the same for both; `update.lifecycle` says what is on offer and staged
  (`kind` is `deployment` or `set`, with `setKind`), and `update.sets.core` the sets held and the
  boots a set on trial has left. micad comes from the
  core set and `mica-deploy` from the root, so micad reads the client's `status` leniently: the
  client may be the newer of the two.
  `InstallLatestUpdate` is the operator's one request for the newest release: the maintenance
  window, a metered link and the check's admission refuse it at once, and the check, fetch and
  install then run in the background through the same gates; it never reboots.
  `mica-update` (or `micad update`) is that request on the device's command line: `status`,
  `check`, `install` and `rollback`, the last two rebooting only with `--reboot`. Nothing
  installs without `install`.
- **Power, reset and recovery.** Reboot and power-off are recorded with their caller before they
  run. A staged reset applies one tier: `configuration`, `application-data` or `full-factory`
  (which keeps identity). `application-data` and `full-factory` also clear DATA/state
  `containerd`, `systemd-units` and `openrc-local` (the user's services, section 3.10), and
  refuse to run while `/run/docker.pid` names a live dockerd, whose storage they would clear
  under it. A board-declared physical recovery action at boot maps to a tier. A
  transient root password lives in `/etc/shadow` until the next boot and in no setting.
- **Service registry.** Other `com.mica.*` names are recorded as they appear and vanish; the name
  rule is `crates/mica-busname`, the rule of `mica-pkgs:docs/mica-pkgs.md` section 3.5.

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
| `docker` | mica-docker | -- |

Failed services are read with mica-openrc's `/usr/lib/mica/mica-init failed`, the log with
`logread`, and power goes through `openrc-shutdown`.

### 3.9 Containers

On a podman product every container is mica-containerd's (`micaoss/mica-podman`, README
*mica-containerd*); micad holds the declarations and hands them over. On a docker product nothing
is declared and micad is a client of dockerd (*On docker* below). A declaration is a **Compose
project**: one Compose file, held to the subset mica-containerd carries out, each service one
container named by its `container_name`, else `<project>-<service>-1`.

**Settings.** `container.enabled` is the switch; `container.projects.<project>` is the project's
Compose text, a string. A project name is 1 to 40 of `[a-z0-9_-]`, starting with a letter or a
digit (mica-containerd's rule); at most 32 projects, each at most 64 KiB, together at most 256
KiB. `container.projectDirs.<project>` is a project's directory, an absolute path: mica-containerd
resolves the project's relative host paths (`./data`, an `env_file`) in it, and a project without
one has `/mica/volumes/<project>/`. A directory of a project that is not declared is refused, and a
write that removes a project removes its directory. `container.json` holds `enabled` and
`projectDirs` and nothing else: one with any other key is a refused document of the `container`
subtree, and a write of `container` replaces it. Each project is its
own file, `/mica/config/container/<project>.yaml` (directory `0700`, files `0600`), written and
removed in the same undo journal as every other document. A file in that directory not named
`<project>.yaml` is ignored; one that cannot be read as UTF-8 text is a refused document of the
`container` subtree. A file changed by hand is read when micad starts.

**A write is checked.** A write that adds or changes a project, its text or its directory, runs
`mica-containerd compose check --project <project> [--project-dir <dir>] -` on its text: no daemon
is needed, so this works with containers off. A refusal is `InvalidArgs` naming the project and
every problem; a check that cannot run is a failed write. `CheckContainerProject` runs the same
check in the project's stored directory and answers its result whole, so a caller can show the
problems field by field; `CheckContainerProjectIn` runs it in a directory it is given.

**The reconciler.** Switched on, it enables and starts `mica-containerd`, waits for it to answer
(30 s), and declares the projects as one set, `PUT /v1/projects`
(`{"projects": {"<project>": "<text>"}}`, or `{"compose": "<text>", "projectDir": "<dir>"}` for
a project with a directory): every project, container and network the daemon
holds outside the set is removed. When this pass started the service, each container of every
project is then started (`POST /v1/containers/<name>/start`): a switch turned off stopped them,
and a stop holds until a start.

Switched off, while the service runs: every project's container (`GET /v1/projects`) is stopped
(`POST /v1/containers/<name>/stop`); once podman lists none of them running (60 s), the service
is stopped and disabled. The projects stay declared, in the settings and in the daemon's store;
turned on again, they are declared and started.

The applied state is `enabled`, `service`, `serviceState`, `declaredProjects` (names), the
daemon's answer (`projects`), `startedContainers` and `stoppedContainers`.

**Bus.** `GetContainers` answers `enabled`, `projects` (each name to its text), `projectDirs`,
`engine` (the daemon's `projects`, each with its `projectDir`, or unavailable with the reason) and
podman's `images`. `StartContainer`,
`StopContainer` and `RestartContainer` take `<project>` (every container of the project) or
`<project>/<service>`; a name the settings do not hold is refused before any call, and a
service's container is the one the daemon's `GET /v1/projects` names for it.
`CheckContainerProject(project, text)` and `CheckContainerProjectIn(project, text, project_dir)`
answer `{"ok": true, "containers": [<spec>...]}` or
`{"ok": false, "detail", "problems": [{"service", "field", "detail"}]}`.
`GetContainerLog(name, tail)` answers the last `tail` lines (1 to 1000) of
`<project>/<service>`, at most 256 KiB.

**On the device: `micad ctr`** (or `mica-ctr`). Root's, over the bus to micad, so it, the console
and the API change one set of declarations:

| Command | Does |
| --- | --- |
| `up [-p <project>] [-f <file\|->] [--project-dir <dir>]` | Checks the file, prints the problems and exits 1 when refused; otherwise stores it as the project with its directory and turns containers on |
| `down [-p <project>]` | Removes the project; its containers and network go, its directory and `/mica/volumes/<project>/` stay |
| `list` (or `ps`) | Every project's containers: project, service, container, phase, health |
| `check [-p <project>] [-f <file\|->] [--project-dir <dir>]` | Checks a file without storing it: the containers it becomes, or the problems |
| `start`, `stop`, `restart <project>[/<service>]` | The verb on the project's every container, or on one |
| `logs <project>/<service> [--tail N]` | The last lines of its log (100 unless asked) |

`up`, `down` and `check` take docker compose's defaults. The file is the first of
`compose.yaml`, `compose.yml`, `docker-compose.yaml` and `docker-compose.yml` in the working
directory unless `-f` names one (`-` is stdin). The project directory is the file's directory
(the working directory for stdin) unless `--project-dir` names one. The project is `-p`, else the
file's top-level `name`, else the project directory's name in lower case with only `a-z`, `0-9`,
`_` and `-`, none of `_` and `-` leading; `down` takes the same name from the working directory.
So `cd /srv/bit-56-core && micad ctr up` declares project `bit-56-core`, its `./data` being
`/srv/bit-56-core/data`. `up` and `down` wait for the reconcile and report it; exit 0 done, 1
failed, 2 misused.

**API.** `GET /api/v1/containers` is `GetContainers`. `PUT /api/v1/container-projects/{project}`
(`{"compose": "<text>"}`) declares or replaces a project, which keeps its directory; a refused file is 422 `compose_invalid`
with `problems` beside `message`. `DELETE /api/v1/container-projects/{project}` removes one.
`POST /api/v1/container-projects/{project}/check` checks a text without storing it.
`POST /api/v1/container-projects/{project}/{action}` and
`.../{project}/services/{service}/{action}` (`start`, `stop`, `restart`) are the verbs;
`GET .../{project}/services/{service}/logs?tail=` the log, `{"log"}`, scrubbed of secrets (200
lines unless asked). The console lists the projects with each service's state and verbs, and
edits a project's text with the check's problems beside it.

**On docker** (`CONTAINER_ENGINE=docker`) there is no mica-containerd, and micad is a client of
dockerd's API on `/run/docker.sock` (version 1.44, the oldest Docker 29 serves) and does nothing of
its own. Containers are made on the device with `docker` and `docker compose`; restart, boot start
and health are dockerd's, from each container's restart policy and health check.

- **The switch.** `container.enabled` turns Docker on and off, as on podman: on, the reconciler
  enables and starts `docker.service` (the OpenRC script `docker`, in the default runlevel) and
  waits 60 s for `/_ping`, and dockerd starts what its restart policies say; off, it stops and
  disables the service. Nothing is declared and no container is started or stopped by micad:
  dockerd runs with `live-restore` off (mica-docker), so stopping it stops its containers, as
  `systemctl stop docker` does, and the next start applies their restart policies.
- **Reads and verbs.** `GetContainers`, `micad ctr list` and the console list every container
  dockerd holds, as it reports it: a container compose made under its project and service (its
  labels), any other as the project and the service of its own name. The phase is dockerd's state
  (`created`, `running`, `paused`, `restarting`, `removing`, `exited`, `dead`); `health`,
  `restarts` and `exit_code` are dockerd's. `StartContainer`, `StopContainer` and
  `RestartContainer` take `<project>` (each of its containers) or `<project>/<service>`, and are
  dockerd's `start`, `stop` and `restart` (10 s before a kill); `GetContainerLog` is its `logs`.
- **No declarations.** A write that adds or changes `container.projects` or
  `container.projectDirs` is refused, and so are `CheckContainerProject(In)` and `micad ctr up`,
  `down` and `check`: they say to use docker compose.
- dockerd starts after micad, so a staged reset runs before it, and neither micad nor the boot's
  health waits for it: a dockerd that is late or absent is a container apply that fails and is
  retried, and the reads answer it unavailable.

`GetContainers` names the engine, `containerEngine`: `podman` or `docker`.

**A reset** of application data empties `projects` and `projectDirs` and clears `/mica/volumes/`
and `/mica/containers/` but its `networks` and `tmp`, which on docker is dockerd's storage.

### 3.10 The user's services

A user's own service -- a systemd unit or an OpenRC script, with its binaries on DATA (in
`/srv`, say) -- is declared in `services.units` and kept on DATA state by Base's `mica-unit`
(`mica-system-base:docs/floor-and-options.md`, *The user's own services*), which micad runs for
every change: one implementation of where it lives, how `[Install]` links it and how the init is
told. `mica-unit` stays on the device for a service micad does not manage.

**Settings.** `services.units.<name>` is `{"kind", "text", "conf"?, "enabled"}`, in
`/mica/config/services.json`. The name is the unit file's under systemd (`x.service`, `x.socket`,
`x.timer`, `x.path`, `x.target`, `x.mount`, a template `x@.service`; addressed quoted,
`services.units."x.service"`) and the script's under OpenRC (`[A-Za-z0-9._-]`, at most 64, no
leading `.`). `kind` is the init it is for, `systemd` or `openrc`; `text` the unit file or the
openrc-run script (its first line `#!/sbin/openrc-run`); `conf` the script's `conf.d` file
(OpenRC only); `enabled` whether it starts at boot. At most 32 services, each text at most 64
KiB, a conf at most 16 KiB, together at most 256 KiB. An enabled systemd unit needs `WantedBy=`,
`RequiredBy=` or `Alias=` in `[Install]`; a template is not enabled. A write that adds or changes
a service is refused when its `kind` is not the product's init, when the root ships its name or
one of its `Alias=` names (`/etc/systemd/system`, `/usr/lib/systemd/system`,
`/lib/systemd/system`; `/etc/init.d`), or when the root has no `/usr/sbin/mica-unit`.

**The reconciler** (after micad claims its bus name, as every reconciler that starts a service):

- A service whose text differs from the installed file is installed (`mica-unit install`, and
  `install --conf` for a changed conf), from a fresh `0700` directory under `/run/mica`.
- Its links follow `enabled` (`mica-unit enable` and `disable`); a changed unit file of an
  enabled systemd service is linked again, for its `[Install]` may name other targets.
- It is started when it becomes enabled and stopped when it stops being so, against the record of
  what micad applied, `/var/lib/mica/services-applied.json`; an enabled service whose text or
  conf changed is restarted. A reconcile that changes nothing about a service does nothing to it:
  the init starts what is enabled at boot (systemd after the bind of
  `/usr/local/lib/systemd/system`, OpenRC through `mica-local`), a oneshot is not run again by an
  unrelated write, and a service a timer runs is not stopped.
- A service the record names and the settings no longer declare is stopped and removed
  (`mica-unit remove`). Only those: a service put there with `mica-unit` by hand stays. Declaring
  the name of one takes it over.
- A service declared for the other init (a restored or pushed document) is stored and not applied,
  and reported in `refused`.
- One that fails is named in the reconcile's error, and the others still apply.

The applied state is `available`, each applied service's `enabled` (and `error`), and `refused`.

**Bus.** `GetUnits` answers `init`, `available` (the root has `mica-unit`) and `units`: each
service declared or in `mica-unit`'s directory, with `declared`, `managed` (declared for this
init), `installed`, `enabled` (linked), the init's `active` state and, for one of the other init,
`refused`. `StartUnit`, `StopUnit` and `RestartUnit` take an installed service, managed or not,
through systemd's manager or `rc-service`; what they do lasts until the next boot or the next
change of `enabled`. `GetUnitLog` answers its newest lines as `GetLog` answers a service's: the
journal of the unit under systemd, and under OpenRC the RAM log's lines tagged with the script's
name, which is what the script sends to syslog. A name that is not installed is `InvalidArgs`.

**On the device: `micad unit`.** Root's, over the bus to micad, like `micad ctr`:

| Command | Does |
| --- | --- |
| `install <file\|-> [--name <name>] [--conf <file>] [--enable]` | Stores the file as a service named after it (`-` reads stdin and needs `--name`), with a conf on OpenRC; `--enable` enables it, and one already enabled stays so |
| `enable`, `disable <name>` | Enables it, which starts it; disables it, which stops it |
| `remove <name>` | Stops it and deletes it |
| `start`, `stop`, `restart <name>` | The verb, now, on any installed service |
| `list` | Every service: whether it is enabled, the init's state, and whether micad or a hand manages it |
| `logs <name> [--tail N]` | Its newest lines (200 unless asked) |

Under systemd a name without a unit type is a `.service`. The writes wait for the reconcile and
report it; exit 0 done, 1 failed, 2 misused.

**API.** `GET /api/v1/units` is `GetUnits`. `PUT /api/v1/units/{name}`
(`{"kind", "text", "conf"?, "enabled"}`) declares or replaces a service, `DELETE` removes one;
`POST /api/v1/units/{name}/{action}` (`start`, `stop`, `restart`) is a verb, and
`GET /api/v1/units/{name}/logs` the log, each line scrubbed of secrets.

**A reset** of application data empties `services.units` and clears the directories
`mica-unit` keeps them in, DATA/state `systemd-units` and `openrc-local`.

## 4. mica-apid

The same executable as micad, run as `mica-apid` or `micad apid`: the API and the console. It holds
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
| `APID_UI_DIR` | `/usr/share/mica-apid/ui` | The built-in console served at `/_ui/` |
| `APID_LOCAL_SOCKET` | `/run/mica/apid.sock` | The API for services on the device |

`--version`, `--openapi` (the committed `crates/mica-apid/openapi.json`, which the gate holds to
the handlers) and `--healthcheck` (for the boot health gate) answer without starting a daemon.

**URL space.** `/api/v1/...` is the API; `/api/versions` lists the versions served; `/healthz` is
liveness; `/_ui/` is the built-in console from `mica-apid-ui` (404 without the package); `/` is
the active custom console bundle or a redirect to `/_ui/`.

**The API**, by group: session, setup and password; API tokens; settings, state and tasks;
network, Wi-Fi, containers, the user's services (`/units`, section 3.10), Bluetooth, MQTT and
SSH keys; observation (time, storage, system
information, telemetry, service logs at `/system/logs/{source}`, network status, health, meta);
actions (reboot, power off, transient root password, WireGuard key rotation); updates (check,
fetch, import of an uploaded `.micaupd`, install, install of the newest release, confirm, reject, rollback, reboot override,
policy); provisioning status and claim; credential recovery and reset; diagnostic snapshots;
console bundles. `GET /meta` lists the product's features, and the console hides what is absent.
`PUT /api/v1/network/{iface}` takes a CAN entry like any other, and the console's network page
edits a CAN interface's bit rate, restart delay, CAN FD and data bit rate.

**Authentication.** Until an admin password exists, `POST /setup` sets it. Browsers exchange the
password for an HMAC-signed, HttpOnly session cookie and send the session's CSRF token on every
mutation; automation uses bearer tokens `mica_<id>_<secret>`, stored as digests. Passwords are
argon2id, failed logins back off persistently, and security-relevant actions go to an audit ring.

**The local socket.** apid also serves the API on `/run/mica/apid.sock`, whatever `access.web`
says, for the device itself. A caller there carries no credential: the kernel's peer credentials
name its uid, the uid names a scope, and the scope names the routes it may call; anything else,
and any uid without a scope, is 403 `not_allowed`. Two scopes:

- root may call the whole API, which is how a command line on the device configures it;
- `mica-bled` (the user of that name in `/etc/passwd`) may call the routes
  `mica-app:docs/protocol.md` section 7.1 tunnels, `GET /api/v1/bluetooth` and
  `PUT /api/v1/settings/bluetooth.pin`.

Both work before the device is claimed. The socket is `0660` with the `mica-bled` group, or
`0600` on a root without one, so nobody else can connect. The forced rotation of a bootstrap claim
binds a local caller like any other.

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

The broker and the bridge are the packages `mica-mqtt-broker` and `mica-mqttd` of mica-pkgs; what
they are is `mica-pkgs:docs/mica-pkgs.md` section 2. micad's side is the contract of its section 3:

- **The broker's configuration**, `/run/mica/mqtt-broker.toml`: exactly `listen_address`,
  `listen_port` and `auth_enabled`, rendered from the `mqtt` settings before the broker starts
  (3.1). Accounts are not micad's: the broker reads them from
  `/var/lib/mica/mqtt-broker-users.toml`.
- **The bridge's device id**, `/run/mica/mqttd-device.env`: the one assignment
  `MICA_MQTT_DEVICE_ID=<id>` (3.2).
- **The services** (3.3): `mica-mqtt-broker.service` and `mica-mqttd.service` on systemd,
  `mica-mqtt-broker` and `mica-mqttd` on OpenRC, started when MQTT is on and stopped when it is
  off. The packages enable neither.

A product carries MQTT when its `FEATURES` has `mqtt` and its root holds both packages.

## 6. Deployments and boot

### 6.1 Storage

| Where | Holds |
| --- | --- |
| SYSTEM (`/mnt/system`) | At most two deployments: descriptors, signatures, content-addressed objects under `kernels/` and `roots/`. At most three sets of each kind under `sets/<kind>/` (6.2) |
| ESP, or the U-Boot environment on a FIT board | The boot records that select a deployment and count trial attempts |
| DATA `/mnt/data/meta` | Transaction state, catalog checkpoints, the firmware receipt |
| DATA `/mica/updates` | Downloads and staged objects; an acquisition never writes SYSTEM |
| DATA project directories | Quotas set by the runkit: `mica` and `srv` project 100, `cache`, `tmp` and `var` project 101 (one eighth of DATA, 32 to 256 MiB, 2048 to 16384 inodes), `containers` project 102 |

SYSTEM and the boot partition are mounted read-only and made writable only inside an operation,
under the transaction lock.

### 6.2 Signed metadata

Strict JSON verified with Ed25519 against keys embedded in the signed kernel; unknown fields are
refused. A signed payload is its canonical form (keys sorted, no whitespace, no duplicate key),
and any other form is refused, so key order is part of the wire form. An `arch` is `amd64`,
`arm64` or `armhf` in every document that carries one (the deployment and its components, a set
and its core records, firmware, the boot policy's identity). `mica/deployment/v1` (a deployment:
its product, its kernel and its root, the system alone), `mica/kernel/v1` and `mica/rootfs/v1`
(components and their verity parameters; a root states its `interfaceLevel`, 1 or more, and may
state the packages it `provides`; a kernel may state its `features`), `mica/update-envelope/v1` and `mica/firmware/v1` (firmware, never
carried by a deployment). A deployment names no core components: those come from the device's
core set, below, and it may name the set its image carries (`coreSet`). Offline archives start with `MICAUPD1` and may carry any subset of a
descriptor's objects.

**Kernel features.** A kernel's optional `features` names what its configuration carries, as the
image producer composed it: words of 1 to 32 lowercase letters, digits and hyphens, sorted and
unique, at most 32 (the producer's are `storage-gpt`, `storage-ubi`, `init-systemd`, `netdev`,
`selinux`, `firewall`, `containers`, `wireguard`, `dm-crypt`). A word the reader does not know is
kept, not refused. Absent, the kernel makes no claim and nothing is refused for it. Present, micad
refuses a VLAN or bridge interface on a kernel without `netdev` and a WireGuard interface on one
without `wireguard`, naming the missing feature, instead of rendering configuration the kernel
cannot run. It reads them from `mica-deploy status`, which reports each deployment's
`kernelFeatures` when its kernel claims any.

**Sets.** `mica/set/v1`, signed with the release key in the same envelope, is the components of
one kind, channel and architecture: `kind` (lowercase letters and digits), `channel` (`general`,
or lowercase letters, digits and hyphens), `arch`, `generation` (monotonic within its kind,
channel and architecture), `version`, and `components`, one per package, sorted. Its id is the
SHA-256 of the signed payload. It is released once and taken by every device of the architecture
on the channel.

A set is the same document, the same catalog line, the same check, fetch and state for every
kind. Four things are a kind's own: what a component record is, which components a device takes,
how a set takes effect, and what makes a pending set good. `kind` is inside the signed payload,
so a set signed as one kind is never taken as another, and a reader refuses a set of a kind it
does not implement. There is one kind, **`core`**: its components are every `mica/core/v1` record
of the channel, each of the set's architecture, with every `needs` met inside the set; a device
takes them by its product's features, composes them at boot, and a set is good when that boot
reaches healthy. What follows is the core kind.

- **Every device has one.** An image carries a seed set as its first, with its components: the
  set its deployment names (`coreSet`, below). A device with no set does not boot: nothing else
  names core components.
- **Declarations.** Whether a set runs on a root is what the two declare, and only that; what
  neither declares is compatible.
  - A core component declares, in its `core/<package>.json` and so in its `mica/core/v1` record:
    `features` (the products that take it), `needs` (other core components, each with `min` and an
    optional `max` version), and two optional constraints on the root. `root` is the interface
    levels it runs on, `{min, max?}`; absent, it runs on every level. `requires` is up to 16 root
    packages, `[{package, min?, max?}]`, with Debian versions.
  - A root declares, in its `mica/rootfs/v1` record: `interfaceLevel` (6.5), and the optional
    `provides`, up to 64 of its packages and their Debian versions, `{"<package>": "<version>"}`.
    mica-build writes `provides` from the root's package database.
  - A `requires` entry holds when the root does not provide the package, or provides it at a
    version from `min` to `max` (inclusive, Debian ordering; an absent bound is no bound).
- **Selection.** A device composes the components of its core set with at least one feature among
  the `FEATURES` of its root's `product.conf`, in package order. The selection is refused when a
  selected component's `root` excludes the root's `interfaceLevel`, when one of its `requires`
  does not hold on the root, or when it needs a package that is not selected or is selected at a
  version outside its range; the refusal names the component and the constraint. Selecting
  nothing is not a refusal. One function (`core_set::select`) is the rule on the device (the
  runkit, `set-check`, `set-install` and `install`), and `set.json` `selections` holds the
  producer's port of it to the same cases.
- **A system update and the core set.** A `mica/deployment/v1` descriptor may name the set its
  image carries, `coreSet`: `{id, channel, generation, sha256, bytes}`, the last two those of the
  signed set's envelope. A release's objects and an update archive then hold that envelope and the
  set's component objects beside the deployment's: a reader takes objects beyond the deployment's
  only when it names a `coreSet`, and holds them to the set once its envelope is read. `fetch`
  and `import` bring the set's objects whenever `install` would take the image's set. `install`
  decides which set the new deployment boots with, before it writes anything:

  | The device's set | The new deployment boots with | `install` refuses when |
  | --- | --- | --- |
  | `general`, of a generation below the image's `coreSet` | The newest `general` set at hand that selects on the new root, never older than the image's: the channel's latest when it is given and selects, else the image's. It is installed with the deployment as one pair | Neither selects |
  | `general`, of the image's generation or a newer one | The device's set, unchanged: a system update never downgrades core | The device's set does not select on the new root |
  | Another channel | The device's set, unchanged: a system update never moves a device off its channel | The device's set does not select on the new root |
  | Any, and the descriptor names no `coreSet` | The device's set, unchanged | The device's set does not select on the new root |

  A latest set that is not a `general` set at or above the image's generation is passed over.
  A refusal names the set and the constraint that failed. **A pair** is on trial together. The
  set is bound to the new deployment and composed whenever that deployment boots. The
  deployment's trial is the only one counted. Confirmation makes both current, and a failed trial
  boots the previous deployment with the set it ran.
- **A core update alone.** `set-check` reads the latest set of the device's channel and selects
  it on the running root before anything is fetched. One that does not select is not offered: the
  check reports it as `waitsForSystem` (`id`, `generation`, the reason). Every check reads it
  again, so it is offered once a system update has brought a root it selects on; on `general`
  that system update may already have taken it as a pair. Every other rule is the one of a set on
  trial below.
- **Sets held.** The current set, the set on trial, and the sets bound to the other deployments
  retained: at most three. Each retained deployment boots the set it was confirmed with. When a
  set becomes current, alone or as a pair, each other retained deployment that is not bound is
  bound to the set it ran. A deployment confirmed again after booting its bound set makes that
  set current again. A binding of a deployment no longer retained is dropped, and a set the state
  does not name is released with its objects.
- **A set archive** is `MICAUPD1` with the signed set as its envelope and the set's objects.
- **State.** SYSTEM `sets/core/<id>.json` holds each signed set. `sets/core/state.json` names
  `current`, at most one `pending`, `pair`, the deployment the pending set is paired with, and
  `bound`, each other retained deployment's set by deployment id (at most two). A pending set with
  a `pair` is composed when that deployment boots, and its trial is the deployment's.
  `sets/core/objects/<component id>/` holds each component (`core.img`, `core.roothash.p7s`,
  `core.roothash`). Only the running system writes them, and a deployment's collection keeps every
  set the state names. DATA `meta/set-trial.json` (`kind`, `id`, `attemptsLeft`) counts the boots
  (three) of a pending set that is not a pair. There is one such file for every kind, so one set is on trial at a time.
  The runkit mounts DATA before it composes, then picks the set:
  - a pair's set, when the deployment it is bound to boots;
  - a deployment's `bound` set, when that deployment boots;
  - otherwise it spends one boot and tries the pending set, or composes `current`.
  It passes over a pending set it cannot authenticate or select from, or whose components are
  not all there at their signed length. A set it must compose and cannot, or none at all, stops
  the boot. It leaves `/run/mica/core.json` (`coreSetId`, `pending`, and `paired` when the set
  on trial is paired with the deployment booted) for the running system: what this boot
  composed, so `pending` stays true for the rest of the boot that committed the set.
- **Commands.**
  - `mica-deploy set-check --kind core --source <root> --channel <c>` and `set-fetch` with the
    same arguments. `--deployment <descriptor>` makes them select on that deployment's root
    rather than the running one, for the set a pair takes.
  - `set-install <set> --objects <dir>` (the kind is the one the set was signed as).
  - `install <descriptor> --objects <dir> [--core-set <set>]`, where `--core-set` is the
    channel's latest, fetched for this deployment.
  - `status` reports the sets under `sets.core`, with `paired` and `bound`.
  - A fetched set is staged as `verified/set-<kind>-<id>.json`. `import` takes either archive
    and names which (`kind` is `deployment` or `set`, with `setKind`).
  - `confirm`, which the health gate calls, makes the booted set current (with its deployment,
    for a pair), or drops a set alone whose trial is spent and a pair whose deployment is no
    longer retained. `fail-boot` with a set alone on trial retires no deployment.
  - A set alone is not installed while a deployment is on trial, and a deployment is not
    installed while a set alone is pending.

The server catalog is **unsigned** and comes in three documents, each fetched only when needed.
The configured source is an **update root** ending in `/` (http or https, a host, no user info,
query or fragment, such as `https://res.micaos.dev/update/`). The reader appends the manifest
major it reads, `v1/manifest.json`:

- **The manifest**, `mica/catalog/v1`: one line per board and product, naming the current
  release's id, generation, notes and `path`. A device that is current, or whose product has no
  line, reads nothing more.
- **The release's document**, `mica/release/v1`: it must agree with its line (id, board, product,
  generation). It names the descriptor (`path`, `sha256`, `bytes`) and each object (`sha256`,
  `bytes`, `path`).
- **The signed descriptor**: it must match that digest and length, authenticate, and name the
  same board, product and generation as the line, the device's arch, and exactly those objects,
  beside the objects of the core set it names (above).

Each document's links are `baseUrl` + `path`. A `baseUrl` ends in `/`, has a host and no user
info, query or fragment, and is https unless the source is http. A `path` stays below its base.
Both unsigned documents are canonical JSON (sorted keys, no whitespace). Trust comes from the
signed descriptor, not from an address: a device takes only its own board, arch and product, only
a generation above the one it runs, and checks every byte against what the descriptor names. The
manifest revision is checkpointed as a consistency check against a confused mirror, not as a
security control; there is no expiry, so a withheld catalog reads as "nothing newer".

The manifest's optional `sets` lists, per kind, channel and architecture, the current set
release: `{kind, channel, arch, latest: {id, generation, path}}`, with `id` the release's name
(`<kind>.<channel>.<stamp>`), the same on every architecture's line. `path` names a
**`mica/set-release/v1`** document
(`{schema, baseUrl, id, kind, channel, arch, generation, set: {path, sha256, bytes}, objects}`),
read as a release's document is: it must agree with its line, the signed set with its digest, its
length and the line's kind, channel, architecture and generation, and `objects` are exactly what
the set names. A release is laid out as `mica/<kind>.<channel>/<stamp>/<arch>/` with `index.json`,
`set.json` and `objects/<sha256>`. A device whose channel has no line, or that holds the line's
generation, reads the manifest and nothing else. A line of a kind the reader does not implement
is passed over.

**Compatibility.** The version is in the path, chosen by the reader, so a device's configuration
never names a format. Within a major, documents only grow: the reader ignores fields it does not
know. A change an older reader cannot ignore gets a new major at a new path (`v2/manifest.json`),
and the server keeps writing the old major for as long as devices read it. The old major's
current release must be a stepping stone whose reader takes both majors. The signed descriptor
refuses unknown fields, so a change to it follows the same rule: first ship a reader that takes
the new schema, then publish it.

The operator
document `/mica/config/updates.json` is `mica/update-config/v1` and the baked defaults are
`mica/meta/v1` in the root (`product`; `update` is `source`, `policy`, `checkIntervalMinutes` and
the optional `channels`, the channel followed for each kind of set as `{"core": "lts"}`, a kind
not named following `general`; `http.credentialHosts`; `fleet`). The operator's document
overrides `policy`, `checkIntervalMinutes`, `source.url` and `channels`, and alone carries
`checkAt`, `source.maxBytes` (the fetch budget, 500 MB by default), `network` (`mode` `online`,
`metered` or `offline`, and `meteredAllowsFetch`), `maintenance.windows` (UTC), `rebootPolicy` and
`rebootGate`. `/mica/config/fleet.json` is `mica/fleet-config/v1` (`enabled`, `reporting`, `url`)
over the baked `fleet`; the provisioning status reports it, and nothing on the device acts on it
yet.

The shared fixtures in `crates/mica-deploy/tests/component-contracts/` (the documents, the
catalog, the refused schemas and cases, the chunker vector and the broken indexes) are generated
by the `component-contract-fixtures` example, signed with test-only keys, and copied by mica-build.

### 6.3 mica-deploy

`/usr/bin/mica-deploy <command>` prints JSON, exits non-zero on failure and bounds every read by
the caller's budget (`--max-bytes`, required by all but `discard`): `status`, `check`, `fetch`,
`import`, `install`, `set-check`, `set-fetch`, `set-install`, `booted`, `confirm`, `reject`,
`rollback`, `fail-boot`, `probe`, `discard`, `gc`, `firmware-readback`.
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
`dataQuotas` (`false` mounts DATA without project quotas), `bootTrust` and `storage` (below).
The section is validated before use, and the disk is held to the stated geometry before anything
is written. The policy is read by field name, in any order; its producer writes it as canonical
JSON, as it writes every document.

`bootTrust`, in the `board` section, says what the boot path proves: `fit` (the default, and
never written) when the loader verifies the FIT's signature, `none` when it only checks the FIT's
hashes. `none` is valid only with `uboot-fit`. The runkit reports the boot as verified
(`bootVerified` in `/run/mica/boot.json`) on UEFI with Secure Boot and on a FIT board whose trust
is `fit`, and as unverified on a FIT board whose trust is `none`; mica-deploy holds every receipt
to that rule. Nothing else changes with
it: the runkit still requires signed dm-verity root hashes, and every deployment, set and
firmware manifest is authenticated against the policy's keys before anything is written. What
`none` gives up is the chain from the loader: someone who can write the flash can replace the
kernel, its initramfs and the keys in it together. Such a board has no verified boot.

`storage`, in the `board` section, is `gpt` (the default, and never written), the block layout
above, or `ubi`, raw NAND (6.6).

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
  has left DATA read-only. The exit ramdisk exists on x86-64 and aarch64 only, so a 32-bit
  board runs an OpenRC root.

**Core components at boot.** Each component the device selects from its core set is opened like
the root, a
signed dm-verity mapping `mica-core-<package>` mounted at `/run/mica-core/<package>`, before
anything binds into the root's trees. The runkit then refuses a composition that would shadow a
file (a component path that is not a directory where the root or another component has it, or
anything outside `/usr` and `/etc`), and mounts `/usr` and `/etc` each as a read-only overlay of
the components' trees, in package order, over the root's own, with no upper layer. The files
appear at the paths a package would have used (`/usr/bin/micad`, `/etc/init.d/apid`, the units
and their enablement links). Shutdown releases the component mappings with the root's.

**The root interface level.** A `mica/rootfs/v1` root states an `interfaceLevel`, and each core
component the levels it runs on (`root: { min, max }`); a core set whose selection does not run
on a root is not composed over it, and a deployment is not installed with a set that does not
select on its root (6.2). The level moves only when something a core component
relies on from the root changes, so an ordinary root update keeps it. Level 1 is:

- glibc and the libraries `micad` links against (`ldd /usr/bin/micad`), from the Base release;
- the init and its service contract: systemd with `multi-user.target`, or OpenRC with
  `supervise-daemon` and the `default` runlevel, and the services of section 3.8 by name;
- `mica-system`, the D-Bus system bus and `/usr/share/dbus-1/system.d`;
- the root's mount points and runtime directories (`/run/mica`, `/var/lib/mica`, `/mica`);
- the packages built into the root from this repository, at interfaces micad calls:
  `mica-deploy` (its command line and JSON);
- the MQTT packages of mica-pkgs, where the product carries them, at the contract of
  `mica-pkgs:docs/mica-pkgs.md` section 3.

mica-build raises the level whenever one of these changes incompatibly, and a core component
raises its `root.min` when it needs the change. A dependency on a root package outside level 1 is
the component's `requires` (6.2), held only on a root that carries that package.

`crates/lifecycle-sys` is the only crate allowed `unsafe`: typed, bounded wrappers for the
watchdog, loop devices, device-mapper, ext4 project ids and quotas (`FS_IOC_FSSETXATTR`,
`quotactl_fd`) and the clock's synchronized bit. The boot/shutdown gate proves every structure
and request number against the kernel headers for x86-64 and aarch64
(`scripts/gate/boot-shutdown/uapi.c`).

### 6.6 NAND boards: the UBI object store

A board whose policy says `storage: "ubi"` keeps its deployments on raw NAND under UBI. The store
is the one of 6.1 and 6.2 (content-addressed, at most two deployments and two core sets, objects
shared between them, the same transactions, records, trials and delta transfer); only where the
large objects live changes, from files on SYSTEM to static UBI volumes. Bad blocks and wear are
UBI's. A reader that does not know `storage` refuses a `ubi` policy rather than misreading it,
because the policy's unknown fields are refused.

**The policy.** On `ubi`, `board` carries `ubi: { "mtd": <name> }`, the MTD partition that holds
the UBI device (1 to 63 of `A-Z a-z 0-9 . _ -`), and neither `partitions` nor `records`; the
policy carries no `systemPartUuid` or `dataPartUuid`. `boot` is `uboot-fit`, `dataQuotas` is
`false` (UBIFS has no project quotas), `reserves.esp` is refused (there is no ESP), and the board
declares its other `reserves` (the defaults are sized for disks). Its `firmware` target is
`mtd-loader`:

```json
{ "format": "mtd-loader", "partition": "<MTD partition name>", "maxBytes": <1 to 64 MiB> }
```

mica-deploy admits a firmware receipt for it and reads it back from the start of the partition,
passing over bad erase blocks, and never writes it: a loader on NAND is replaced by the recovery
package. Every other MTD region the board's first-stage loader fixes is the image producer's;
the device knows only the UBI partition.

**Volumes.** The names are fixed; an id is the lowercase hexadecimal SHA-256 id of its signed
document (6.2).

| Volume | Kind | Holds |
| --- | --- | --- |
| `system` | dynamic, UBIFS | SYSTEM's small files: `deployments/<id>.json`, `roots/<rootId>/rootfs.roothash{,.p7s}`, `kernels/<kernelId>/support.roothash{,.p7s}`, `sets/core/` (the signed sets, `state.json`, `objects/<componentId>/core.roothash{,.p7s}`) |
| `data` | dynamic, UBIFS | DATA |
| `records-a`, `records-b` | dynamic, one LEB each | The two boot record copies |
| `k-<kernelId>` | static | The kernel's `boot.itb` |
| `s-<kernelId>` | static | The kernel's `support.img` |
| `r-<rootId>` | static | The root's `rootfs.img` |
| `c-<componentId>` | static | A core component's `core.img` |
| `p.<name>` | static | The object `<name>` while it is written |

A static volume's data size is exactly the signed length of its object. An object is written
under the transaction lock as a file is (6.3): a leftover `p.<name>` is removed, `p.<name>` is
created static at that size, filled through the volume update (`UBI_IOCVOLUP`), read back whole
through its character device and checked against the signed digest, then renamed to `<name>` in
one volume-table update. An existing `<name>` is reused only when it is static, neither corrupted
nor marked mid-update, of the signed size and of the signed digest. Collection and `gc` remove
volumes (`UBI_IOCRMVOL`) as they remove files, after the records stop naming them: every `p.*`,
and every `k-`, `s-`, `r-` or `c-` volume no retained deployment or set names. No other volume is
touched. An object costs its size rounded up to whole LEBs; installation keeps `reserves.system`
free in the UBI device's available LEBs, and a download keeps `reserves.data` on DATA. Delta
transfer reads its seeds from the object volumes' character devices.

**Records.** The format is the FIT one, unchanged: a copy is 65536 bytes, the CRC-32 of bytes 5
onward in bytes 0 to 3, a flag in byte 4, then `mica_entries=v1|...` and zeros, and the active
copy is chosen by U-Boot's redundant-environment rule. Copy A is the first 65536 bytes of
`records-a`, copy B of `records-b`. For every reader, a copy whose volume cannot be read whole
(corrupted, or marked mid-update) is an invalid copy, as one whose CRC fails, so the other copy
holds. Every writer changes LEB 0 of the inactive copy's volume atomically with 65536 bytes:
mica-deploy with the atomic LEB change (`UBI_IOCEBCH`), micaboot with U-Boot's `ubi_leb_change`,
which has the same semantics. Neither uses a volume update, which unmaps every LEB first. An
interrupted write leaves the old copy or the new one; mica-deploy reads a copy back before it
counts.

**Boot.** The kernel's built-in command line carries `ubi.mtd=<name>`, the policy's `ubi.mtd`
(and `mtdparts=` when the partitions are not in the device tree), and the kernel attaches UBI
before init; a FIT board's kernel forces its built-in command line, and the loader adds nothing.
The runkit attaches nothing: it takes the UBI device whose MTD partition is the policy's, mounts
`ubiN:system` read-only and `ubiN:data` read-write without quotas, and opens the root, the
support image and each selected core component as a `ubiblock` of its static volume (checked
static, sound and of its signed size) under a signed dm-verity mapping, as it opens a loop device
on a disk board. A UBI board gets no exit ramdisk whatever its init: there is no loop device or
disk to release, and the root's own shutdown leaves UBIFS read-only before it reboots; the
shutdown helper accepts a `ubiblock` under a MICA mapping as it accepts a loop device. A
confirmed deployment that failed is retired in the records volumes. mica-deploy requires
`/mnt/system` and `/mnt/data` to be the UBIFS mounts of `ubiN:system` and `ubiN:data` of that UBI
device, and remounts SYSTEM writable only inside an operation, as on a disk. micad reports the
`system` and `data` tiers as those volumes, of role `ubifs` and without partition sizes.

**micaboot over UBI.** The loader arms the watchdog; attaches the UBI partition; reads
`records-a` and `records-b` (`ubi read`), chooses the active copy and the next deployment by the
same rules as on a disk board; when the chosen record is on trial, decrements its attempts and
writes the result to the inactive copy (`ubi_leb_change`, above) before launching anything,
launching nothing if that write fails; reads `k-<kernelId>` whole (`ubi read`, at most its FIT
limit); checks the FIT's hashes (`verify=yes`) and, with `bootTrust` `fit`, its signature; sets
`/chosen/mica,deployment-id` to the deployment's id; and boots it. A confirmed deployment that
fails to launch is retired in the records as on a disk board.

**Kernel options.** A UBI board's kernel carries MTD with its SPI-NAND (or NAND) controller and
`MTD_SPI_NAND`, `MTD_UBI`, `MTD_UBI_BLOCK`, `UBIFS_FS`, `SQUASHFS` with the compressors of the
images it opens, `BLK_DEV_DM`, `DM_VERITY` with `DM_VERITY_VERIFY_ROOTHASH_SIG` and the
system trusted keyring holding the root-hash signing key, and a watchdog driver; all built in,
since nothing is loaded before the root.

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
  crypto library (rustls, digests, HMAC, random, Ed25519, X25519). The password hashes are their
  own crates: argon2id for the admin password and the device credential, bcrypt for the transient root password, the hash
  `/etc/shadow` and dropbear's `crypt(3)` read.
- AWS-LC seeds from the kernel's `getrandom` alone: `.cargo/config.toml` builds it with
  `AWS_LC_SYS_NO_JITTER_ENTROPY=1`, because its default CPU-jitter seed fails its health test on
  coarse counters (the RK3576's 24 MHz) and AWS-LC then aborts the process on its first draw.
  `micad-settings` checks it.
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
| `make package-gate` | Pool rules (names, architecture, no commit stamp, no path in two packages, copyright, no conffiles, POSIX maintainer scripts); the declared packages and core components at their versions and no other item, each core record an unsigned `mica/core/v1` naming its own image, exact dependency versions, enablement links, nothing outside `/usr` and `/etc` in a component, component hash trees that verify, and one producer per pool rebuilt on an empty-cache builder to the same bytes (the core components' on the first pool) |

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
| `deploy` | `mica-deploy` | `mica-deploy` |
| `lifecycle` | `mica-runkit` (static) | `mica-lifecycle` |

`scripts/deb/build.sh` builds one producer for one architecture: its `prepare.sh` compiles the
binaries with `scripts/build/build-deb.sh` in the rust image of the host architecture (native on
an arm64 runner, cross-compiled otherwise, never emulated), into a target directory of its own,
and its `Dockerfile` packs them with `mica-tools deb pack` in the base image of the target
architecture, so `${shlibs:Depends}` is resolved against the target's libraries. A core component is staged and
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

`ci.yml` runs on every push to `main` and every pull request: `make deps`, `make check` and
`make dbus-policy-test`, then a native amd64 and arm64 build, pack and package gate with its
rebuild, then both pools gated together without a rebuild and every archive put to
`mica-tools pool guard`. It publishes nothing.

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
