# MQTT: mica-mqtt-broker and mica-mqttd

MQTT on a Mica OS device carries **application data only**. System
configuration, state, health, credentials, updates and power never become MQTT
items; they stay on the management plane (micad and apid).

```mermaid
flowchart LR
    client[MQTT client] <-->|MQTT 3.1.1| broker[mica-mqtt-broker]
    mqttd[mica-mqttd] <--> broker
    mqttd <-->|D-Bus: GetItems, ItemsChanged, SetValue| app["enrolled com.mica.* application"]
    micad -. renders /run/mica/mqtt-broker.toml<br/>and /run/mica/mqttd-device.env .-> broker
    micad -. starts and stops both units .-> mqttd
    mqttd -. no access .-x micad
```

## 1. Switching it on

Both daemons ship disabled. The `mqtt` settings subtree decides:

| Setting | Default | Effect |
| --- | --- | --- |
| `mqtt.enabled` | `false` | Whether the broker and the bridge run |
| `mqtt.listen.address`, `mqtt.listen.port` | `127.0.0.1`, `1883` | The broker's single listener; a wider bind is a deliberate operator change |
| `mqtt.auth.enabled` | `false` | Whether a client must authenticate; the accounts are read from `/var/lib/mica/mqtt-broker-users.toml` on STATE, never from the settings tree |

apid serves the subtree as one resource, `GET/PUT /api/v1/mqtt`, with the
reconciler's live state beside it; `mqtt.enabled` is also writable on its own
through `PUT /api/v1/settings/mqtt.enabled`, which is what the console's
service switch uses.

micad's `mqtt` reconciler renders `/run/mica/mqtt-broker.toml` and
`/run/mica/mqttd-device.env` (the validated device identity only) before it
brings `mica-mqtt-broker.service` and `mica-mqttd.service` to the requested
state.

## 2. mica-mqtt-broker

`crates/mica-mqtt-broker`. rumqttd used as a library: it reads the three
mica-owned keys (listen address, listen port, whether authentication is on),
builds the rumqttd configuration in code and serves one MQTT 3.1.1 listener.
There is no HTTP console, no metrics listener, no clustering and no bridging, and
none can be enabled by editing a file on the device. It runs as the
`mica-mqtt-broker` user, created by the package's `postinst`.

## 3. mica-mqttd

`crates/mica-mqttd`, running as the `mica-mqttd` user.

### 3.1 What it bridges

Only **enrolled** application services. Each file name in
`/usr/lib/mica/mqtt-applications.d/` is one exact D-Bus service name
`com.mica.<class>[.<suffix>]`; the application package that owns the service
installs that file and the D-Bus policy grant for `mica-mqttd`. The bridge
knows each service's application surface — `GetItems`, `ItemsChanged`,
`SetValue` — and nothing else. It has zero D-Bus access to `com.mica.micad`.

How a service name maps to a class is `crates/mica-busname`, the rule micad's
service registry uses too.

### 3.2 Topic grammar

```
N/<deviceId>/<class>/<instance>/<path>   published by the bridge: {"value": ...}
R/<deviceId>/<class>/<instance>/<path>   a read request
W/<deviceId>/<class>/<instance>/<path>   a write request: {"value": ...}
```

A keepalive triggers a rate-limited full republish, terminated by
`full_publish_completed`; the bridge sends a heartbeat every 3 seconds.

### 3.3 Modes

| Mode | Behaviour |
| --- | --- |
| `read-only` (default) | Publishes `N` topics; `W` topics are not subscribed and are refused if they arrive |
| `full` | `W` topics become `SetValue` on the uniquely addressed item of an enrolled service |

The unit reads `MICA_MQTT_MODE`, the broker host and port and the client id from
its defaults, overridable in `/var/lib/mica/mqttd.env` on STATE, and the device
id from `/run/mica/mqttd-device.env`.

### 3.4 Design

The protocol is a pure state machine (`bridge.rs`): events in (keepalive, items
changed, request, tick, service vanished), effects out, with time passed in as
an explicit monotonic clock. Two traits sit around it — `Transport` (the broker,
rumqttc in production) and `ItemSource` (the application buses) — joined by
`runtime.rs`. The protocol tests drive the production state machine, encoder
and masking against an in-memory transport.

## 4. The reference service

`crates/mica-mqtt-reference` is a deliberately small `com.mica.Item1` service
with a fixed item tree, used to prove the application boundary on a test
image. It is not packaged.
