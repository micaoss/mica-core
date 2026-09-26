# 20260926-1000-features-decide-the-surface The product's features decide which API a device serves

- **status**: implementing
- **approvedAt**: 2026-09-26 (user: "能力开放这个可以按计划来处理")
- **createdAt**: 2026-09-26 10:00
- **relatedTask**: 20260926-1000-features-decide-the-surface

## Context

Every device serves every route and runs every reconciler, whatever the product carries. A board
without a radio still answers `/api/v1/wifi/*` and `/api/v1/bluetooth/*`, and the answer is an
error from a daemon that is not there ("unavailable", "no adapter"); the console shows the tabs and
then the errors. The same holds for SSH, containers and MQTT.

The device already has an authenticated statement of what the product carries:
`/usr/lib/mica/product.conf` in the dm-verity root, written by the assembly, holds
`FEATURES="..."` (today `micad mqtt containers wifi bluetooth` on the FIT boards and
`micad mqtt containers` on the UEFI ones). mica-core reads the `PRODUCT=` line of that file already
and ignores `FEATURES`.

## Proposal

### 1. One reading of the features

A `Features` value parsed from `FEATURES=` in `product.conf`, shared by micad and apid (in
`micad-settings`, which both link). The words mica-core acts on:

| Feature | What it switches |
| --- | --- |
| `wifi` | the Wi-Fi client and access point: reconcilers, `ScanWifi`, `/api/v1/wifi/*`, settings `wifi` |
| `bluetooth` | the Bluetooth reconciler and pairing agent, the Bluetooth bus members, `/api/v1/bluetooth/*`, settings `bluetooth` |
| `ssh` | the SSH reconciler, `/api/v1/ssh/*`, the transient root password action, settings `access.ssh` |
| `containers` | the container reconciler, `/api/v1/containers*`, settings `container` |
| `mqtt` | the MQTT reconciler, `/api/v1/mqtt`, settings `mqtt` |

Other words (`micad`, a board's `display`, ...) are ignored. Hostname, network, time, updates,
access, diagnostics, reset and the console are always present.

A root with no `product.conf`, or one with no `FEATURES` line, is a development host: every feature
is on, so `cargo run`, the tests and a dry-run daemon behave as they do today.

### 2. micad: what is not in the product does not run

- The reconciler list is filtered by the features: an absent feature's reconciler is not
  registered, so nothing renders its files or drives its units.
- A settings write into an absent feature's subtree is refused, naming the feature, so a
  configuration for hardware the product does not carry cannot be stored and resurface later.
- Its bus members answer one named refusal (`feature not in this product: wifi`) rather than the
  error of a missing daemon. The Bluetooth agent is not registered.
- `GetFeatures` (or a `features` member of the existing system information) reports the set.

### 3. apid: routes that are not in the product are not served

- An absent feature's routes are not mounted: they answer the router's own 404 `not_found`, like
  any path the API does not serve, before authentication is consulted for them.
- `GET /api/v1/meta` gains `features`: the set the device serves, so a client knows without
  probing.
- The OpenAPI document stays whole; each gated operation says which feature it needs.

### 4. The console hides what is not there

The console reads `features` from `/api/v1/meta` and leaves out the Wi-Fi and Bluetooth tabs, the
container and MQTT panes and the SSH section when their feature is off, instead of rendering them
and then an error.

### What mica-build does in the same release

`ssh` is a new word: dropbear is in every root today and no product names it. The products that
carry SSH must list `ssh` in `FEATURES` in the commit that pins this release, or SSH management
disappears from their API. Moving dropbear out of the floor into a `feature-ssh` package set, so a
product without `ssh` does not carry it, is the other half and mica-build's to decide.

## Risks

- **A product that forgets `ssh`** loses its SSH API and its key reconciler; dropbear keeps
  whatever keys it had. The coordination above is the mitigation; the counterpart commit must
  land with the pin.
- **Stored settings for a feature a later product drops** (a device updated to a product without
  `wifi`): the subtree stays on disk and is inert, because no reconciler reads it and no route
  writes it. It is not deleted, so a product that brings the feature back finds it.
- **Hardware presence is separate.** A product with `bluetooth` on a board whose adapter did not
  probe still serves the routes, and they report the adapter as unsupported, as today. Features
  say what the product carries; the observers say what the hardware answered.

## Scope

In: `micad-settings` (the reading), `micad` (reconciler filtering, settings refusal, bus members),
`mica-apid` (router, meta, OpenAPI notes), the console, docs. Out: mica-build's product
definitions and a `feature-ssh` package set (counterpart).

## Alternatives

- **Probe the hardware instead** (`/sys/class/net/*/phy80211`, `/sys/class/bluetooth`): no
  product-level choice (a board with a radio that a product does not use), and it cannot say
  anything about SSH or MQTT. The observers keep doing this for what they report.
- **Serve everything, report unavailable** (today): what this replaces.

## Annotations

- 2026-09-26 (user): "加一个开关，因为我们有一些板卡可能没用无线或者蓝牙，或者ssh，因此需要读一个系统能力，
  这样来决定我们开放哪些api，不是默认全部开启然后去报错".
- 2026-09-26 (user): SSH is already an optional package, `mica-ssh` in mica-system-base; the
  feature word is `ssh`, and mica-build lists it in `FEATURES` for a product that carries
  `mica-ssh`.
- 2026-09-26 (implementation): the transient root password is **not** gated by `ssh`. It is the
  way into a device with no key at its physical console too, and the console is an option of its
  own, separate from `mica-ssh`.
- 2026-09-26 (implementation): no `GetFeatures` member. apid reads `product.conf` itself through
  the same `micad_settings::Features`, so the router is decided at start without a bus round trip,
  and the two daemons read one file with one parser.
- 2026-09-26 (implementation): the OpenAPI note is added by the generator (`openapi.rs`
  `note_features`) from one table of path prefixes, rather than repeated in 23 handler docs.
- 2026-09-26 (implementation): the console hides a pane only once `meta` has answered without its
  feature; while `meta` is loading or unreadable everything is shown, and the routes answer for
  themselves.
