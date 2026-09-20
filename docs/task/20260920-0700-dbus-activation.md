# 20260920-0700-dbus-activation Nothing mica-core ships is D-Bus activated

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 07:00

## Question

`/usr/lib/dbus-1.0/dbus-daemon-launch-helper` is setuid in the Base root and
absent from the composed product root, one of 626 paths no composition rule
claims. It is how `dbus-daemon` spawns a service itself for *traditional*
activation. Is anything on a Mica OS device D-Bus activated rather than started
by a unit?

## Answer: no, and the helper is not on the path for what is activated either

**1. mica-core ships no activation file.** `git grep system-services` over
`pkgs/`, `crates/` and `scripts/` returns nothing. The only `dbus-1` path any
producer installs is `/usr/share/dbus-1/system.d/com.mica.micad.conf`
(`pkgs/micad/Dockerfile`), which is **policy** -- who may own and call the name
-- not activation.

**2. Every daemon is unit-started.** `micad.service` is `Type=dbus` with
`BusName=com.mica.micad` and `WantedBy=multi-user.target`: systemd starts it and
waits for the name to appear, which is the inverse of activation. `apid.service`,
`mica-mqttd.service` and `mica-mqtt-broker.service` are `Type=simple`,
`WantedBy=multi-user.target`, and own no bus name at all.

**3. Nothing calls a name expecting it to start.** apid calls `com.mica.micad`;
with no activation entry the bus answers `NameHasNoOwner` at once and apid maps
it to `micad_unreachable` (503 with `Retry-After`). `scan.rs` only watches
`NameOwnerChanged` under `arg0namespace='com.mica'`, which starts nothing.

**4. The `org.freedesktop.*` names micad does call are activated by SYSTEMD, not
by the helper.** micad calls `systemd1`, `hostname1`, `timedate1`, `network1`,
`resolve1` and `timesync1`. Measured in the pinned `systemd 257.13-1~deb13u1`
(`mica-system-base:locks/upstream.lock` line 312, downloaded and verified
against that row, sha256 `dcc3ba37…`): it ships six activation files, and five
of them -- `hostname1`, `locale1`, `login1`, `network1`, `timedate1` -- carry

```
Exec=/bin/false
User=root
SystemdService=dbus-org.freedesktop.hostname1.service
```

`SystemdService=` means `dbus-daemon` asks **systemd** to start the unit and
forks nothing, so the setuid helper is never used; `Exec=/bin/false` is a
deliberate dead fallback. The sixth, `org.freedesktop.systemd1.service`, has no
`SystemdService=` because PID 1 owns that name from boot.

## What this does and does not settle

It settles mica-core: dropping the helper is **unreachable by construction** for
everything this repository ships or calls, and that is a decision that can be
recorded rather than a defect nobody looked at.

It does not settle the composed root as a whole. A package elsewhere could ship
a *traditional* activation file -- one with `Exec=` and no `SystemdService=` --
and that one would need the helper. The check is one line and belongs with
whoever holds the 626-path list: in the product root, every file under
`/usr/share/dbus-1/system-services/` must carry `SystemdService=`; any that does
not either needs the helper back or needs a reason.

## The gap in our own gate

`make dbus-policy-test` covers **policy only**: it starts a `dbus-daemon` with
the shipped `com.mica.micad.conf` and tests who may own, call and receive. It
has no activation case, so it would not notice an activation file appearing in
one of our packages, nor a consumer starting to rely on activation. Worth
adding the day either becomes possible; today neither is.

## ActiveForm

Establishing whether anything is D-Bus activated

## Dependencies

None. The composed-root check is mica-build's, with the criterion stated above.
