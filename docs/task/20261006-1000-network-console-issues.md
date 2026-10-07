# 20261006-1000-network-console-issues Network: console defects and gaps from device testing

- **status**: done
- **priority**: P1
- **owner**: mica-core
- **createdAt**: 2026-10-06 10:00

## Description

Nine items reported by the user after testing the network pages on a device
(2026-10-06). Investigated in the source; nothing was run on a device, so the
items marked *needs device evidence* name a likely cause, not a confirmed one.

## Findings

| # | Report | Finding | Kind |
|---|---|---|---|
| 1 | The interface list shows addresses that are not valid | `network-page.tsx` `summarize` joins networkd's address bytes with `.`, so an IPv6 address prints as sixteen dotted numbers, and link-local addresses are listed with the rest | defect |
| 2 | DNS cannot be set on a DHCP interface | `dns` exists only inside `static`; a DHCP entry has no field for it | gap |
| 3 | A physical interface used as a gateway offers no DHCP pool | The model, apid and the renderer accept `dhcpServer` on any statically addressed interface. The form shows it only once the mode is static and an address is typed, with no hint in DHCP mode | discoverability |
| 4 | Saving eth0 / br0 fails with `Cannot read properties of undefined (reading 'join')`, then the page shows "Something went wrong!" | apid omits `dhcpServer.dns` when it is empty (`skip_serializing_if`), and `interface-detail.tsx` calls `serverRow.dns.join`. Enabling the DHCP server with no DNS, the default, saves an entry the page can no longer render | defect |
| 5 | An SSID that was added is not on the air; several SSIDs per interface | One access point (`wifi.ap`), 2.4 GHz only, mode `off` by default; `provisioning` starts it only without an uplink. The radio cannot be an access point and a station at once. Multi-SSID is not modelled | needs device evidence; gap |
| 6 | Wi-Fi scan fails: `wpa_supplicant could not be asked: Resource temporarily unavailable (os error 11)` | EAGAIN is the 1 s receive timeout of `wpa_query`: wpa_supplicant never answered. The client socket is bound in the temporary directory, which a sandboxed wpa_supplicant cannot reply to; the reply buffer is 4096 bytes | likely defect, needs device evidence |
| 7 | A bridge cannot take eth0 / wlan0: `network.br0 has bridge port "wlan0", which is not a declared network entry` | `validate_topology` requires every port to be a declared entry with no addressing. The form offers observed interfaces that are not declared, and writes only the bridge | defect (eth); gap (wlan) |
| 8 | A phone or a computer gets no pairing code | The agent is registered once, when micad starts. When bluetoothd starts later, or restarts, no agent is registered and nothing answers a pairing | likely defect, needs device evidence |
| 9 | The local peer block of a WireGuard interface is on one line | The block is built with newlines and shown in `CopyField`, a single-line field | defect |

## Proposal

Defects, in one change:

1. Addresses: format IPv4 and IPv6 from the bytes, and list global-scope
   addresses only; link-local stays on the interface's own page.
3. Show the DHCP server section in DHCP mode as a disabled hint that says it
   needs a static address.
4. apid always serializes `dhcpServer.dns`; the page also tolerates its
   absence. A regression test renders an entry saved with no DNS.
6. Bind the control client socket under `/run/mica`, raise the reply bound for
   `SCAN_RESULTS`, and read a reply larger than 4096 bytes.
7. Saving a bridge declares each undeclared port (`dhcp: false`, no address)
   in the same settings write, and the review dialog says which interfaces
   lose their addressing. A wireless interface is not offered as a port.
8. Register the agent whenever `org.bluez` appears on the bus, not only at
   start.
9. Show the peer block as a multi-line block with a copy button.

Decisions needed before any work on them:

- 2: a `dns` list on a DHCP entry that replaces the servers the lease gives
  (networkd `UseDNS=no` + `DNS=`; resolv.d on OpenRC).
- 5: several SSIDs on one radio, and an access point bridged into a bridge
  (hostapd `bridge=`), instead of its own address and DHCP server.
- 6: joining an existing network already exists (`wifi.client`); bridging a
  station into a LAN needs 4-address mode on both ends and is not proposed.

## Acceptance

- Each defect has a test that fails before the fix.
- `bash scripts/gate/rust-gate.sh` and the UI checks are green.
- Items 5, 6 and 8 are confirmed on a device, with the unit state and logs.

## Decisions (user, 2026-10-07)

- 2: do it. A DHCP entry takes `dns`, servers used instead of the lease's.
- 5: no multi-SSID and no bridged access point: the SSID is for provisioning,
  not a general hotspot. `provisioning` mode starts it only without an uplink.
- 6: no bridging of a station into a LAN.

## Outcome

- **1**: `listedAddresses` spells IPv4 and IPv6 from networkd's bytes and
  leaves loopback and link-local out of the list.
- **2**: `network.<iface>.dns` on a DHCP entry (refused on any other). networkd:
  `DNS=` with `UseDNS=no` for DHCPv4, DHCPv6 and router advertisements.
  OpenRC: udhcpc asks for no DNS option (`-o -O subnet -O router -O
  broadcast`) and the servers go into `/run/mica/resolv.d/00-<iface>`, which
  sorts before the lease's file, so they come first even from a server that
  names its own unasked.
- **3**: a DHCP entry shows the DHCP server section as a hint that it needs a
  static address.
- **4**: two causes, both fixed. The page called `.join` on a `dhcpServer.dns`
  the device does not write when empty; and `PUT /api/v1/network[/{iface}]`
  answered 204 while the page read a task id from the body. Both routes now
  answer **202** with `TaskAccepted`, like the container routes; the page's
  tests stubbed 202, which is why they never saw it.
- **6**: confirmed in the source. `micad.service` has `PrivateTmp=yes`, the
  reply socket was bound in the temporary directory, and wpa_supplicant and
  hostapd answer by sending to that path, which does not exist outside
  micad's namespace. The socket is bound under `/run/mica`; the reply buffer
  is 64 KiB. This also restores Wi-Fi status and the access point's stations
  on systemd products.
- **7**: saving a bridge declares the ports it gains (`dhcp: false`, no
  addressing, routes or DHCP server) in one `PUT /api/v1/network`, and the
  review names them. A radio is not offered as a port.
- **8**: confirmed in the source. The reconciler starts bluetoothd when
  `bluetooth.enabled` turns on, after the one registration at start had
  failed. `keep_agent_registered` registers on every `NameOwnerChanged` that
  gives `org.bluez` an owner.
- **9**: `CopyField` keeps line breaks.
- Every producer is 0.0.4.

Still to confirm on a device: 5 (which mode the access point was in), and
that 6 and 8 behave as the source says.
