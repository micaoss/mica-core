# What a Mica OS device can do, and where that lives

An index, not a design document: one row per capability, pointing at the crate
that implements it and the page that explains it. Nothing here restates how a
subsystem works -- follow the link for that. The columns are the two questions
a reader arrives with: *which code owns this* and *where is it written down*.

## Management plane

| Capability | Owned by | Explained in |
| --- | --- | --- |
| Declared settings, per-document on DATA, with a schema version per document | `crates/micad-settings` | [design/micad.md](design/micad.md) §3 |
| Reconcilers: render, compare, write, drive systemd | `crates/micad/src/reconciler/` | [design/micad.md](design/micad.md) §4 |
| Read-only observation of the running system | `crates/micad/src/{network_state,time_status,storage,telemetry,health}.rs` | [design/micad.md](design/micad.md) §5.2 |
| The D-Bus interface (`com.mica.micad1`, root only) | `crates/micad/src/bus.rs` | [design/micad.md](design/micad.md) §5 |
| The HTTPS API and the console it serves | `crates/mica-apid` | [design/apid.md](design/apid.md) |
| Task records for asynchronous writes | `crates/mica-apid/src/task_registry.rs`, micad's apply loop | [design/apid.md](design/apid.md) §3 |

## Device capabilities

| Capability | Owned by | Explained in |
| --- | --- | --- |
| Hostname | `reconciler/hostname.rs` | [design/micad.md](design/micad.md) §4 |
| Network interfaces: physical, VLAN, bridge, WireGuard; static routes; per-interface DHCP server | `reconciler/network.rs`, settings `network` | [design/micad.md](design/micad.md) §4 |
| Wi-Fi client: known networks, scanning, association facts | `reconciler/wifi_client.rs`, `wpa_client.rs` | [design/micad.md](design/micad.md) §4 |
| Wi-Fi access point: hostapd, connected stations | `reconciler/wifi_ap.rs` | [design/micad.md](design/micad.md) §4 |
| Bluetooth: adapter, pairing agent, trust list | `reconciler/bluetooth.rs`, `bluetooth.rs` | [design/micad.md](design/micad.md) §4.4 |
| Containers: declared units rendered by Quadlet, observed through podman | `reconciler/container.rs`, `containers.rs` | [design/micad.md](design/micad.md) §4.5 |
| MQTT broker and the application-data bridge | `crates/mica-mqtt-broker`, `crates/mica-mqttd`, `reconciler/mqtt.rs` | [design/mqtt.md](design/mqtt.md) |
| SSH access: dropbear, authorized keys | `reconciler/sshd.rs`, settings `access.ssh` | [design/micad.md](design/micad.md) §4 |
| Time: NTP servers, the presentation timezone, synchronization state | `reconciler/time.rs`, `time_status.rs` | [design/micad.md](design/micad.md) §4, §5 |
| SFTP for application data | `crates/mica-sftp-server` | [architecture.md](architecture.md) |

## Lifecycle

| Capability | Owned by | Explained in |
| --- | --- | --- |
| Signed deployments: check, fetch, import, install, confirm, reject, rollback | `crates/mica-deploy` | [design/deployment.md](design/deployment.md) |
| Update policy: cadence and its optional time of day, maintenance windows, the safe-to-reboot gate | `crates/micad/src/update_policy.rs`, `update_auto.rs` | [design/deployment.md](design/deployment.md) §3.1, [design/micad.md](design/micad.md) §7 |
| Early boot and shutdown | `crates/lifecycle-sys`, mica-runkit | [design/deployment.md](design/deployment.md) §4 |
| Provisioning, claim and the device credential | `crates/micad/src/provisioning.rs`, `identity.rs` | [design/micad.md](design/micad.md) §6 |
| Reset, by tier | `crates/micad/src/reset.rs` | [design/micad.md](design/micad.md) §8 |
| Diagnostics snapshots | `crates/mica-apid/src/diagnostics.rs` | [design/apid.md](design/apid.md) §6 |
| Console bundles: uploaded, activated, rolled back | `crates/mica-apid/src/bundle.rs`, `crates/mica-ui-bundle` | [design/apid.md](design/apid.md) §5 |

## Packaging

| Capability | Owned by | Explained in |
| --- | --- | --- |
| Seven Debian producers, their versions and the inputs guard | `pkgs/`, `scripts/deb/` | [design/packaging-and-release.md](design/packaging-and-release.md), [../pkgs/README.md](../pkgs/README.md) |
| Gates: Rust, shell, D-Bus policy, boot/shutdown, locks, release | `scripts/gate/` | [design/packaging-and-release.md](design/packaging-and-release.md) |
