# Provisioning, from first boot to a claimed device

The path an unclaimed device takes to a network and an administrator: what each step writes and
which reconcilers it runs. The BLE protocol itself is `docs/ble-provisioning.md`; the access
point's key is `docs/factory-ap-key.md`; settings, documents and reconcilers are
`docs/mica-core.md` sections 3.3 and 3.4.

Where the settings named here live: `wifi.*` in `/mica/config/wifi.json`, `network.*` in
`/mica/config/network.json`, `hostname` in `/mica/config/system.json`, `access` (the
administrator password and the claim record) and `provisioning` in `settings.toml` on STATE.
Every write is persisted first and then applied as a task; the task runs each reconciler whose
subtree, or a subtree it reads, the written paths reach: `wifi.*` runs `wifi_client`, then
`wifi_ap`, and `dns`; `network` runs `network` and `dns`; `access` runs `sshd` and `web`;
`hostname` runs `hostname` and `dns`.
Nothing in an apply writes settings back: a reconciler that cannot converge reports it in its
live state and leaves the tree as it is.

## 1. First boot

`provisioning.state` is not `complete`, so micad seeds the tree once (`crates/micad/src/provisioning.rs`):

- `provisioning.deviceId`, the identity; `hostname` `mica-<id>` when the image left the default;
  `access.ssh.enabled` false.
- On a product with Wi-Fi whose tree declares no network to join and no access point:
  `wifi.ap.mode` `always` and `wifi.client.enabled` false.
- `provisioning.state` `complete`. Seeding never runs again; a later boot loads the tree as it
  was last written.

Every boot then applies the whole tree. On a product with Bluetooth, before the access point
first starts, micad scans once for the networks nearby with a supplicant of its own and keeps the
list for a BLE session's `scanWifi`; the access point starts after the scan. The access point
comes up under the label's SSID and key (`pskSource` `derived`). micad logs the device code and writes `/run/mica/device-code.issue`
for the login prompt, with the console's URL at each address the device holds.

## 2. Unclaimed

The device is unclaimed while `access.webAdmin` is unset. At every boot of an unclaimed device
the BLE window `unclaimed` opens for 10 minutes (auth `none`). The console answers only
`POST /api/v1/setup` (section 4).

## 3. A BLE session

A session starts at `start` and ends with its connection. What it changes is held until the
session is over: the hold keeps the radio's role from before a `setWifi` (`wifi.client.enabled`,
`wifi.ap.mode`), the networks that `setWifi` added, and the wired entries a `setNetwork` replaced.
While a hold exists, a console write reaching `wifi` answers `423 wifi_locked`.

**`setWifi`.** It writes `wifi.client.networks` (the network first, by priority),
`wifi.client.enabled` true and `wifi.ap.mode` `off` in one write. The apply runs `wifi_client`,
which starts the supplicant, and `wifi_ap`, which stops the access point. The device then waits
for the station to associate with that network and hold an address.

- `connected`: the entry **came up**, and is committed at that moment. It leaves the hold: no
  later end of the session, finished or not, puts the radio's role back or removes the network.
- `failed` (or still `connecting` when the session ends): the entry stays in the hold.

**`setNetwork`.** It writes the `network.<iface>` entry's addressing; the apply runs `network`.

- `up`: the entry came up and is committed; it leaves the hold.
- `failed`, or not up yet: it stays in the hold.

A later `setWifi` after one that connected holds the role it then finds (client, on the first
network), so its failure goes back to the first network, not to the access point.

**`finish`** ends the session and returns the facts (`finished`). It commits nothing more.

**The end of the hold.** It ends at `finish`, when the device is claimed (section 4), 2 minutes
after the phone left without returning, or when the window closes. Then, for what is still held:

| The session | Radio role (`wifi.client.enabled`, `wifi.ap.mode`) | Networks it added | Wired entries |
| --- | --- | --- | --- |
| finished, or authenticated (`password`, `temp-code`) | put back | kept | kept |
| unclaimed (`none`) and not finished | put back | removed | put back |

Each put-back is one write of the paths named, applied as above. What came up was taken out of
the hold, so none of this touches it.

The hold lives in micad's memory. A reboot ends it without putting anything back: the device
boots with what was last written.

## 4. The claim on the console

`POST /api/v1/setup` (`docs/mica-core.md` section 4), from any network or the access point:

1. The device code and the login backoff gate it, unless the product turns the code off
   (`CLAIM_DEVICE_CODE=off`, `docs/mica-core.md` section 3.2); `409` once a password exists.
2. `hostname` when the body has one: one write, applied by `hostname` and `dns`.
3. `network` entries when the body has them: merged into the current map, one write, applied by
   `network` and `dns`. The console's setup page sends none.
4. The whole `access` subtree, with `webAdmin` and the claim record, as one `Claim` write; the
   apply runs `sshd` and `web`.

The claim writes nothing under `wifi`: the radio's role, the networks and the access point's key
stay as they are, and the label key keeps applying until one is set (`docs/factory-ap-key.md`).
It seeds no defaults and applies no board network. micad then ends any BLE session of the
unclaimed device (`closing` `claimed_elsewhere`) and its hold, which puts back only what did not
come up (section 3).

## 5. Claimed

The device opens the BLE windows of a claimed device (`boot`, `offline`, `manual`), with
`password` or `temp-code` sessions held as in section 3. The console writes `wifi` and `network`
through their routes; the access point runs when `wifi.ap.mode` is not `off`, and an access point
on the station's interface reports `conflict` instead of starting.

## 6. Wired only

A device whose first network is a cable, with or without a radio, takes this path; a product
without Wi-Fi and Bluetooth has no other.

1. Every wired port (`eth*`) the map does not declare runs DHCP (`docs/mica-core.md` section
   3.4). With no DHCP server on the wire, networkd gives the port a `169.254.0.0/16` address too;
   on OpenRC that fallback is Base's `mica-udhcpc` (section 3.8).
2. The login prompt, on a screen or the serial console, shows the device code and a
   `Console: <url>` line for each address the device holds and for `<hostname>.local`, refreshed
   within 5 seconds of a lease or a link-local address.
3. Cabled to a router, the device is at its lease's address (the prompt, or the router's lease
   list) or at `<hostname>.local`. Cabled straight to a laptop, both ends take link-local
   addresses and the console is at the device's `169.254` address from the prompt.
4. The claim is section 4: the console's setup page, with the device code from the label or the
   prompt, or without one on a product that turns the code off.

## The paths, end to end

| Path | Ends as |
| --- | --- |
| BLE Wi-Fi `connected`, `finish`, claim | client on that network, access point off |
| BLE Wi-Fi `connected`, phone leaves without `finish`, claim (within 2 minutes or later) | the same |
| BLE Wi-Fi `failed` or still connecting, phone leaves, 2 minutes | access point on, the network removed |
| BLE wired `up`, phone leaves without `finish` | the wired entry stays; the access point stays on |
| BLE Wi-Fi `connected`, no claim, reboot | client on that network, access point off; the `unclaimed` window opens again |
| Claim from the access point, no BLE | access point on with the label key |
| Cable, no BLE, claim from the address on the prompt | the port on DHCP, or on its link-local address with no server |
