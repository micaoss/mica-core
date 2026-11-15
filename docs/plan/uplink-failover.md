# Uplink reachability and failover

Status: approved by the user (uplink-20261115), with the answers in section 13. Round 1 is implemented on the 0.0.9 line; round 2 is next.

Facts below are from mica-core main 0c114a5. mica-system-base origin/main was read only.

## 1. Where things are today

- **Default routes** (docs/mica-core.md 3.3, 3.4):
  - Every DHCP uplink micad renders gets its own metric: 100 for the first in name order, then 10 more for each next one. The Wi-Fi client's lease gets 600.
  - A static gateway keeps networkd's metric 0.
  - On OpenRC the same numbers go into each stanza as `metric <n>`. Base's `mica-udhcpc` sets `ip -4 route replace default via <router> dev <if> metric <n>` on `bound` and `renew`.
  - `internet: false` takes no gateway.
  - So the kernel uses the lowest metric. It falls over only when a link goes down or a lease goes away.
- **Reachability**: the only check is the resolution probe, `GetDnsProbe` (network_state/probe.rs). It resolves one name through resolved every 60 s and is display only. An uplink whose link is up but whose upstream is dead keeps the traffic.
- **Route writes**: micad already writes routes over rtnetlink for the OpenRC WireGuard tunnels (netlink/tunnel.rs), with route protocol 109 (`RTPROT_MICA`) and golden-byte tests. It deletes only routes of that protocol.
- **Privileges**: micad runs as root with no capability bounding set (crates/micad/dist/micad.service, micad.openrc). Raw ICMP sockets and `SO_BINDTODEVICE` need nothing new, and no package is needed.
- **DNS**:
  - systemd: resolved holds each link's lease servers. A name with no routing domain goes to every link resolved counts as a default-route link, in parallel, and the first good answer wins. A dead uplink's servers cost nothing visible, but they are still asked.
  - OpenRC: Base's `mica-resolv` concatenates `/run/mica/resolv.d/*` in file-name order into resolv.conf. glibc tries the servers in that order, 5 s each. A dead uplink whose name sorts first delays every lookup.
  - With global DNS on, both inits use only the global servers.
- **OpenRC renewals**: `mica-udhcpc` flushes the interface's addresses on every `bound`/`renew` (`ip -4 addr flush`). That drops every route through the interface, whoever added it.

## 2. Design in one paragraph

micad never changes or withdraws the routes networkd and `mica-udhcpc` own. Each uplink keeps its own default route at its own metric, as today. micad probes every uplink that has probing on, out of that uplink's own interface. When the uplink the kernel would pick (the lowest metric, "natural") is down and a lower-ranked one is up, micad adds ONE route of its own: a default route via the best uplink that is up, protocol 109, metric 10. That route sits above every other default route. When the natural uplink is up again, or no probed uplink is up, micad removes its route, and the kernel's own order applies again. So micad only ever steers traffic toward an uplink it has seen work. It cannot make the device worse off than today: when every uplink fails its probes, it does nothing.

## 3. Mechanism: why an override route

The options considered:

| Option | Verdict |
| --- | --- |
| Raise the down uplink's metric (delete it and add it again with a higher metric) | Rejected. networkd adds its route back at the lease's renewal and whenever the link reconfigures, and `mica-udhcpc` does `route replace` at `renew`. micad would fight both, each renewal. |
| Withdraw the down uplink's default route | Rejected for the same reasons. It would also take the route the probe itself needs: a probe bound to the interface needs that interface's default route to reach a remote target. |
| Rewrite the uplink's metric in its units and reconfigure (`networkctl reconfigure`, `ifdown`/`ifup`) | Rejected. It restarts DHCP on a link that is up, takes seconds, and is two code paths. |
| Per-uplink tables plus policy rules | Rejected. More moving parts on both inits, and networkd's `ManageForeignRoutingPolicyRules` would need a Base drop-in. |
| **Override route, metric 10, protocol 109, only while needed** | **Proposed.** Nothing networkd or udhcpc owns is touched. The route exists only during a failover, and is removed when no longer needed. |

Details:

- **Metric plan.** Override 10. Static gateways move from 0 to 20, 21, ... (name order). DHCP stays at 100 + 10n, Wi-Fi client at 600. A static gateway still beats every DHCP uplink, as today, and the override beats everything.
  - On systemd the static gateway is rendered as `[Route] Gateway= Metric=`. On OpenRC it becomes an `up ip route add default via ... metric ...` line with its `down` partner, as the static routes already are.
  - A declared `0.0.0.0/0` route with a `metric` below 20 is refused (`route_metric_reserved`).
  - No devices are in the field, so the change needs no migration.
- **Ownership.**
  - The override is the only default route micad writes. It is told apart from the tunnels' protocol-109 routes by destination `0.0.0.0/0` on a non-tunnel link (the tunnels never carry a default route; the apply refuses one).
  - At start micad deletes any protocol-109 default route it finds, so a crash cannot leave a stale override. The engine then decides afresh.
- **Re-assertion.** micad subscribes to rtnetlink's IPv4 route and link groups. When its route disappears, it adds it back within one event, as long as it is still wanted. That covers:
  - `mica-udhcpc`'s address flush on renew (the natural route is used for those milliseconds);
  - networkd dropping foreign routes when it reconfigures the link;
  - an operator's `ip route del`.

  A periodic check at each probe round backs the subscription up.
- **Link and lease events.** Carrier loss, a released lease or a missing default route on an uplink is "no route". That is an immediate verdict, with no threshold. The kernel already moves traffic off a link that went away. The engine only follows that.

## 4. Probing

- **Out of the probed interface, on both inits.**
  - Each probe socket is bound with `SO_BINDTODEVICE` to the uplink and to the uplink's IPv4 address as source.
  - The kernel's lookup for a bound socket takes routes out of that device only. So the uplink's own default route (metric 100, 110, ...) carries the probe whatever the active default is.
  - No policy routing is needed, and networkd and ifupdown see nothing.
  - Replies come back on the same interface. This needs reverse-path filtering loose (2) or off (0). Neither root sets it today: the systemd root's /usr/lib/sysctl.d has no rp_filter line (Base's amd64-systemd rootfs), and networkd sets none, so both keep the kernel's 0. A strict value (1) is detected and reported as `rpFilterStrict` on the uplink, and probing there reports `blocked` rather than silently failing (section 9).
- **Methods.** Both are implemented in micad, with no package and no new capability:
  - `icmp`: echo request over a raw ICMP socket (CAP_NET_RAW, which micad has as root).
  - `tcp`: a connect to `port`, closed at once. It needs no capability.
- **Default targets (the user's choice).**
  - `1.1.1.1` and `8.8.8.8`, both `tcp` on port 53. A write that switches the check on with no targets stores these two, so the stored entry names what the device contacts.
  - Port 53, not 443: both resolvers serve DNS over TCP on 53, while a captive portal or transparent proxy commonly answers a connect to any 443, which would make a dead uplink look up.
  - TCP rather than ICMP: many upstreams filter or rate-limit ICMP, and a handshake proves the path the device's traffic takes.
  - The user may edit them or add `gateway` (an `icmp` echo to the uplink's router is cheapest, but proves only the first hop).
- **Targets.**
  - 1 to 4 per uplink, each `{ "address": "<IPv4>" | "gateway", "method": "icmp" | "tcp", "port": 1..65535 }`.
  - `port` is only for `tcp`; it defaults to 443 on a target that names none (the default targets name 53).
  - `gateway` is the uplink's own router: the lease's, or the static gateway.
  - Addresses only: no host names (section 8).
- **One round.**
  - Every target is tried at once, each bounded by `timeoutMs`.
  - The round succeeds when ANY target answers, so one dead target does not take an uplink down.
  - Each target's answer (ok, latency, or why not) is kept for display.
- **Defaults and bounds.**

| Field | Default | Bounds |
| --- | --- | --- |
| `intervalS` | 10 | 2 to 300 |
| `timeoutMs` | 2000 | 200 to 10000, and below `intervalS` |
| `failuresToDown` | 3 | 1 to 20 |
| `successesToUp` | 5 | 1 to 50 |

  With the defaults, a dead upstream is noticed in about 30 s, and an uplink must answer for about 50 s before it takes traffic back.
- **Cost.** About 1.5 MB a day per ICMP target at the defaults. That is tiny, but it is not nothing on a metered LTE uplink. The docs say so, and the console shows it next to the interval.

## 5. State machine and selection

- **Per uplink.**
  - States: `off` (probing not on), `unknown` (on, no verdict yet), `up`, `down`, `noRoute` (no carrier, no lease or no default route), `blocked` (strict rp_filter).
  - Transitions:
    - `unknown`/`down` → `up` after `successesToUp` consecutive good rounds;
    - `unknown`/`up` → `down` after `failuresToDown` consecutive failed rounds;
    - any state → `noRoute` at once on a link or lease loss;
    - `noRoute` → `unknown` when the route returns, so it must earn `up` again.
  - The counters reset on every change of state.
- **Selection.**
  - The candidates are the uplinks the kernel holds a default route for, by their metric.
  - Natural = the lowest metric.
  - Eligible = state `up`, `unknown` or `off`. An unprobed uplink is trusted, as today, and an uplink that has just started probing is not punished before its first verdict.
  - Active = the lowest-metric eligible candidate.
  - Active == natural: no override. Otherwise: the override via active.
  - No candidate eligible: no override. This is the "never withdraw the last uplink" rule taken to its end: micad never withdraws anything. When every uplink looks down, the device behaves exactly as today. So a probe target that is itself down, failing every uplink at once, cannot cut the device off.
- **Hysteresis.** It comes from the two thresholds. A better-ranked uplink that recovers takes the traffic back only after `successesToUp` rounds. One that fails is left only after `failuresToDown` rounds. No extra timer.
- **Scope.** `internet: false` interfaces, WireGuard, CAN and access-point interfaces are never candidates. Probing on them is refused (section 6).

## 6. Settings

`network.<iface>.probe`, on physical, bridge and VLAN entries and the radio's (Wi-Fi client) entry. Absent means off. `enabled` keeps the targets while switched off, as `dns.global` does.

```json
"eth0": {
  "dhcp": true,
  "probe": {
    "enabled": true,
    "targets": [
      { "address": "1.1.1.1", "method": "tcp", "port": 53 },
      { "address": "8.8.8.8", "method": "tcp", "port": 53 }
    ],
    "intervalS": 10,
    "timeoutMs": 2000,
    "failuresToDown": 3,
    "successesToUp": 5
  }
}
```

The numeric fields are optional; absent means the default. Validation codes, each at its field path:

- `probe_not_allowed`: on an `internet: false`, `wireguard` or `can` entry.
- `probe_targets`: more than 4, or duplicates (none, while switched on, stores the defaults).
- `probe_address`: not an IPv4 unicast address or `gateway`; loopback, multicast, `0.0.0.0` and the broadcast address are refused.
- `probe_method`: not `icmp` or `tcp`.
- `probe_port`: out of range, or given with `icmp`.
- `probe_interval`, `probe_timeout`, `probe_threshold`: outside the bounds.
- `route_metric_reserved`: a declared `0.0.0.0/0` route with a metric below 20.

A probe change is a `settings.apply` task: no unit is rewritten, and the engine takes the new settings. The metric change for static gateways is a `network.apply` as today.

## 7. State, events, API, console, CLI

- **Observed state.**
  - `GetObservedNetwork` (and `GET /api/v1/network/status`) gains `uplinks`:
    - `active`, `natural` and `override` (bool);
    - `lastChange`: `{at, from, to, reason}`;
    - per uplink: `{name, metric, state, since, failures, successes, probe: {enabled, intervalS, ...}, lastRound: {at, ok, targets: [{address, method, port?, ok, latencyMs?, detail?}]}}`.
  - `GetNetworkState` stays the networkd `Describe` view; the new fields go where the observation already reports default routes.
- **Event.**
  - A D-Bus signal `UplinkChanged(json)`, with the same body as `lastChange` plus `active`.
  - A log line `MICA_UPLINK active=<if> previous=<if> reason=<probe|link|recovered|none-up>` (journal or syslog).
  - apid has no event stream today. The console polls the observation, so the signal serves bus clients, `mica` and later telemetry.
- **DNS follows the active uplink.**
  - systemd: micad calls resolved's `SetLinkDefaultRoute(ifindex, false)` for an uplink that is `down` while another is active, and `true` when it is up again. Each lookup then goes to the active uplink's servers. The value is runtime only. networkd pushes a link's DNS again on renewal; the route monitor's re-assertion re-applies micad's value after that, as for the route.
  - OpenRC: Base change needed (section 11). `mica-resolv` builds resolv.conf in the order micad writes to `/run/mica/resolv.order` (the active uplink first, down uplinks last), and micad runs `mica-resolv` after each change. micad never writes the files in `resolv.d`, which are `mica-udhcpc`'s.
  - Global DNS on: nothing to do; the servers are reached through the active route.
  - Static servers on a static entry follow their entry like a lease's.
- **Console.**
  - The network page shows per uplink a reachability badge (reachable / unreachable / checking / not checked / no route) and the active marker. It also shows the last round's targets.
  - The interface editor gets a "Reachability check" section with the targets, pre-filled with the two defaults and editable (or `gateway` added), the four numbers and the traffic cost line. Its help says plainly that switching it on makes the device contact those addresses. The check is off by default.
- **`mica network`.** It gains an `INTERNET` column (`up`, `down`, `checking`, `-`, `no route`) and a `*` on the active uplink. `mica network show <if>` prints the probe settings, the state, its counters and the last round's targets. `--json` is the observation's `uplinks`.
  - No write verb now. `mica` cannot change interface entries yet, and a probe is part of the entry, written through the network route with its validation. Adding `network.<if>.probe` alone to `mica set` would be a second write path to one entry. That belongs with a later `mica network set`, if wanted.
- **Docs.**
  - docs/mica-core.md: 3.3 (settings, the metric plan), 3.4 (network reconciler: static gateway metric; new uplink engine paragraph), 3.6 (observation), 3.15 (`mica network`), and the API section.
  - OpenAPI regenerated.

## 8. What I would cut

- **Host-name targets.** A name must be resolved through the uplink being probed. resolved can do that per link (`ResolveHostname` with an ifindex). OpenRC's resolver cannot. Addresses only, on both inits, keeps one behavior.
- **IPv6.** Default routes from router advertisements, and probing over IPv6. The same design applies (override at an IPv6 metric, ICMPv6 and TCP). It comes after IPv4 has run, as its own round.
- **WireGuard tunnels as uplinks**, and a default route over a tunnel.
- **An HTTP check** (captive portals answer TCP 443; section 9).
- **Load balancing, multipath and per-flow policy** (out of scope as asked).
- **A dedicated CLI write verb** (section 7).

## 9. Risks

- **A probe target down for every uplink.** Every uplink goes `down`, nothing is eligible, the override is removed, and the device runs as today. The console shows "unreachable" on all of them. The docs recommend two targets on different networks.
- **A wrong "up".** A captive portal or transparent proxy answers a TCP SYN on 443, so an uplink that cannot really reach the internet looks up. Mitigation: `icmp` to an outside address, or `tcp` to port 53 of a public resolver. An HTTP check is the later fix (cut above).
- **A wrong "down".** An upstream that filters ICMP. Any-of rounds and the TCP recommendation cover it.
- **Strict rp_filter (1).** Replies to probes on a non-active uplink are dropped. Detected per interface (`/proc/sys/net/ipv4/conf/<if>/rp_filter` and `all`) and reported as `blocked`. A blocked uplink is not eligible; the probes must be fixed, since a guess would be worse. Both roots keep the kernel's 0 today; the VM case checks it.
- **Established connections.** A switch does not move flows already open: their source address is the old uplink's, and they stall until the peer gives up or the application reconnects. This is expected and documented. New flows take the new route. Containers and access-point clients behind NAT get the new uplink's address on new flows only.
- **networkd interplay.** networkd's foreign-route cleanup can remove the override when it reconfigures the link. Re-assertion handles it. The mica-build systemd suite confirms it on a real image (section 10).
- **OpenRC renew gap.** The address flush drops the override for the time `mica-udhcpc` takes to rebuild the lease, a few milliseconds. During that time the natural (down) route is used. The re-assertion closes it on the route event.
- **Flapping upstream.** It is bounded by the thresholds. At worst one switch every `(failuresToDown + successesToUp) × intervalS` (80 s at the defaults).

## 10. Tests

- **Unit (micad):**
  - the state machine: every transition, the counters, `noRoute` and `blocked`;
  - the thresholds at their bounds;
  - selection: natural up, natural down with a lower one up, all down, unprobed uplinks, a static gateway among DHCP ones, the Wi-Fi client;
  - settings validation for every code;
  - the observation JSON;
  - the CLI table and `show`.
- **Netlink golden bytes:** the override's `RTM_NEWROUTE` and `RTM_DELROUTE` (main table, protocol 109, metric 10, gateway, output interface), the protocol-109 default-route dump filter, and the route and link group subscription. Same style as netlink/tunnel/tests.rs.
- **Probe sockets:** an ICMP echo and a TCP connect bound to a device, against a veth peer in a namespace. Part of the VM case, since it needs root.
- **Core VM case** (ignored test, booted in QEMU like the tunnels' vm.rs):
  - Two "ISP" namespaces, each behind a veth pair. Each has an upstream namespace holding the probe target, reached over a second veth.
  - The device side gets each uplink's default route at 100 and 110, as networkd and `mica-udhcpc` put them.
  - Cutting ISP A's upstream veth (A's link stays up) makes the override appear via B, and traffic to the target leaves by B.
  - Restoring A removes the override after `successesToUp` rounds.
  - Cutting both leaves no override.
  - An address flush on B, as a renew does, gets the override re-added.
  - A kernel with `rp_filter=1` reports `blocked`.
- **Real inits (mica-build, section 11):** the same topology with two NICs per QEMU product. systemd-networkd on uefi-x64 and OpenRC on mini-x64, each with real DHCP servers in the ISP namespaces. It covers lease renewals during a failover, a network apply during a failover (networkd reconfigure), DNS following the active uplink on both inits, and the `UplinkChanged` signal.

## 11. Cross-repo (requests, not edits)

- **mica-system-base:** `mica-resolv` takes the order from `/run/mica/resolv.order` when it exists (one interface name per line; files not named follow in name order). Until Base releases that, OpenRC DNS keeps today's order and the docs say so.
- **mica-build:** the two-uplink QEMU suite on both inits (section 10), after core's round 1 release.

## 12. Rounds and effort

| Round | Content | Effort |
| --- | --- | --- |
| 1 (core) | Settings model and validation, with the API model and OpenAPI for `probe`; the stored default targets; the metric plan (static gateways 20+n, override 10) on both inits; probe sockets; state machine; selection; override route writes with golden bytes; route monitor and re-assertion; observation `uplinks`; `UplinkChanged` and the log line; unit tests; the core VM case; docs/mica-core.md 3.3 to 3.6. | about 3 days |
| 2 (core) | DNS following (resolved on systemd; the order file on OpenRC once Base has it); the status schema in OpenAPI; console status and editor in both locales; `mica network`; docs. | about 2 days |
| 3 (build, Base) | Base's `mica-resolv` order; mica-build's two-uplink suites on both inits. | Base small; build about 1 day |

Each core round gates once and is pushed on the 0.0.9 line. A release follows only on the user's word.

## 13. The user's answers

1. Static gateways move from metric 0 to 20+n: agreed.
2. Defaults 10 s interval, 2 s timeout, 3 failures to down, 5 successes to up: agreed.
3. Default targets: `1.1.1.1` and `8.8.8.8` by `tcp` on port 53 (section 4). The console pre-fills them and lets the user edit them or add `gateway`; the settings store them explicitly once the check is on. The check stays off by default, and the docs and the console's help say that switching it on makes the device contact those addresses.
