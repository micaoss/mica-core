# 20260920-0812-bluetooth-pairing Pairing, as a declared trust list over BlueZ

- **status**: completed
- **createdAt**: 2026-09-20 08:12
- **approvedAt**: 2026-09-20 09:00
- **relatedTask**: 20260920-0813-bluetooth-pairing
- **completedAt**: 2026-09-20 09:24

## Context

Device testing asked for Bluetooth pairing. The console today reports one fact
about Bluetooth: whether the board has an adapter, read from
`/sys/class/bluetooth` by `radio_evidence` and published under
`capabilities.bluetooth`. Nothing else exists -- no settings subtree, no
reconciler, no bus member, no route.

What the device has, from `micaoss/mica-boards` and `micaoss/mica-build`:

- boards that declare the `bluetooth` capability select the `mica-bluetooth`
  producer, whose package brings `bluez`: `/usr/libexec/bluetooth/bluetoothd`,
  `/usr/bin/bluetoothctl`, `bluetooth.service` and `/etc/bluetooth/*.conf`
  (`rootfs/runtime/consumers.json`);
- `s905x5m` additionally persists its derived adapter address in
  `DATA/state/bluetooth`, so identity across boots is a board concern already
  solved.

So the daemon is there when the board has a radio. What is missing is
everything above it.

**Pairing is not a read.** It is a negotiation with a timeout, a user decision
in the middle (confirm this passkey), and a result that has to outlive the
reboot. That shape is why this is a plan and not a route.

## Proposal

### 1. What is declared, and what is observed

The split every subsystem here takes. **Declared** (`bluetooth` settings
subtree, carried by a new `bluetooth.json`):

```rust
pub struct BluetoothSettings {
    pub enabled: bool,           // whether bluetoothd runs at all
    pub discoverable: bool,      // whether this device answers scans
    pub alias: Option<String>,   // the name it advertises; absent derives one
    pub pin: Option<String>,     // the legacy pairing code; absent derives one
    pub devices: BTreeMap<String, PairedDevice>,  // by address
}

pub struct PairedDevice {
    pub name: String,            // what it called itself when it paired
    pub trusted: bool,           // reconnects without asking again
    pub blocked: bool,
}
```

**Observed** (`GetBluetooth`, read from BlueZ over the system bus): the
adapter's powered/discovering state, and every device BlueZ knows with its
address, name, RSSI, paired/connected/trusted flags. Declared and observed are
named apart, never merged -- a device paired and out of range, and a device in
range and not paired, are different facts.

### 2. The reconciler owns the adapter, not the pairing

A `bluetooth` reconciler, in the shape all eight others take:

- `bluetooth.service` to the state `enabled` asks for;
- the adapter's `Powered`, `Discoverable` and `Alias` properties set from the
  subtree through `org.bluez.Adapter1`;
- every declared device reconciled to its `Trusted` and `Blocked` flags, and a
  device BlueZ holds that the settings tree does not declare **removed**
  (`Adapter1.RemoveDevice`), which is what makes the declared list the truth
  and a reset meaningful.

It does not pair. Pairing needs a live agent and an operator, and a reconcile
pass is neither.

### 3. Pairing is an action with an agent behind it

Three bus members, all of them actions:

| Member | Does |
| --- | --- |
| `StartBluetoothDiscovery` / `StopBluetoothDiscovery` | `Adapter1.StartDiscovery`, with a bounded auto-stop so a device left discovering is not left discoverable forever |
| `PairBluetoothDevice(address)` | `Device1.Pair`, under a timeout, then records the device in `bluetooth.devices` |
| `ConfirmBluetoothPasskey(address, accept)` | Answers the agent's pending `RequestConfirmation` |
| `RemoveBluetoothDevice(address)` | Drops the declaration; the reconciler removes it from BlueZ |

micad registers an `org.bluez.Agent1` at `/com/mica/bluetooth/agent` with
capability `DisplayYesNo`. When BlueZ calls `RequestConfirmation`, micad
records the pending passkey in live state and **blocks the agent's reply** on
`ConfirmBluetoothPasskey` or a timeout. That is the only stateful thing in the
design and it is bounded: one pending request per adapter, refused after 60
seconds, and refused outright when nothing is discovering.

`RequestPinCode` and `RequestPasskey` are answered with **the device's own
PIN**, which is a setting: `bluetooth.pin`, editable, and shown in the console
so an operator knows what to type on the peer. A headless appliance has no
keypad to enter a peer-chosen code on, so the alternative to a device PIN is
refusing every legacy peer outright.

**The default is derived from the device identity, not a constant.** The same
rule the access point's key takes (`WifiApSettings::psk`: "A fleet-wide
constant default is forbidden"), and for the same reason: `0000` on every
device is one PIN for the whole fleet. Derived from the identity and not from
a credential, because this value is displayed by design -- it is a
pairing code, not a secret, and it must be readable to be typed. It is fixed
per device, stable across boots, and an operator who wants `0000` can set it.

**A PIN is not authentication.** Legacy pairing with a known code authenticates
nothing; what bounds it is that pairing is only possible while discovery is on,
which is an operator action with a timeout. The console says so where the PIN
is shown.

### 4. apid and the console

```
GET    /api/v1/bluetooth                 declared + observed, not merged
PUT    /api/v1/bluetooth                 the adapter: enabled, discoverable, alias
POST   /api/v1/bluetooth/discovery       { "on": true | false }
POST   /api/v1/bluetooth/devices/{address}/pair
POST   /api/v1/bluetooth/devices/{address}/confirm   { "accept": true | false }
DELETE /api/v1/bluetooth/devices/{address}
```

The console's page: the adapter switch and its alias, a discovery button with
the found devices, a pairing dialog that shows the passkey and takes the
confirmation, and the paired list with trust and removal.

### 5. Order of work

1. Settings model, `bluetooth.json`, validation (address shape, alias length).
2. The reconciler: unit, adapter properties, declared-device flags, the sweep.
3. The BlueZ observer (`GetBluetooth`).
4. The agent and the four actions, with the pending-request bound.
5. apid routes and OpenAPI.
6. The console page.
7. Docs, changelog, `micad` and `mica-apid` version bumps.

## Risks

- **A board without the capability must not grow a failing unit.** The
  reconciler has to report `unsupported` when `capabilities.bluetooth` says
  there is no adapter, rather than trying to start `bluetooth.service` and
  failing every reconcile. This is the first reconciler whose subject is
  optional hardware.
- **The agent is a resident D-Bus object with a blocking reply.** Every bound
  above is there because of that: one pending request, a timeout, and a refusal
  when discovery is off.
- **Pairing writes settings from an action**, which no other action does today
  (`RotateWireguardKey` deliberately does not). The device list has to be
  written under the same apply lock every settings write takes, or a pair
  landing during a reconcile can be lost.
- **Reset tiers.** A paired phone is operator data: `application-data` should
  clear `bluetooth.devices`, the same conclusion the container work reached.
- **Two packages move**, so both producer versions bump.

## Scope

Comparable to the container work: a settings document, a reconciler, an
observer, four bus members with a resident agent, six routes and a console
page. The BlueZ interfaces are reached with `zbus`, which micad already uses;
no new dependency.

## Alternatives

- **Shell out to `bluetoothctl`.** It is an interactive REPL with a pairing
  agent of its own; driving it from a daemon means feeding a pty and parsing
  human output, and its agent would answer confirmations micad never saw.
  Rejected.
- **Pair from the console over a WebSocket to BlueZ.** Puts the agent in the
  browser and the trust decision outside the device. Rejected: the pairing
  agent is a device-side security decision.
- **Declare paired devices only, with no pairing action** (an integrator pairs
  over SSH once). Half the request, and it needs a shell on a device that
  deliberately has none.

## Annotations

- 2026-09-20: raised from device testing ("蓝牙有配对功能").
- 2026-09-20: user accepted the design and decided the PIN question: a fixed
  PIN is wanted, it must be editable, and the default is shown. Implemented as
  `bluetooth.pin` with an identity-derived default rather than a fleet-wide
  constant -- the access point's key rule, applied to the one value here that
  is displayed rather than hidden.
