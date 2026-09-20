# 20260920-0813-bluetooth-pairing Bluetooth pairing on the device

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 08:13

## Description

Device testing asked for Bluetooth pairing. The device reports one Bluetooth
fact today -- whether the board has an adapter -- and has no settings subtree,
no reconciler, no bus member and no route for anything else.

BlueZ is present where it matters: a board that declares the `bluetooth`
capability selects `mica-bluetooth`, which brings `bluetoothd`, `bluetoothctl`
and `bluetooth.service`. What is missing is the management plane above it.

The design is `docs/plan/20260920-0812-bluetooth-pairing.md`: the paired
devices are declared settings, the reconciler owns the adapter and the trust
flags, and pairing is an action backed by an `org.bluez.Agent1` micad
registers -- with the passkey confirmation bounded to one pending request, a
timeout, and a refusal when nothing is discovering.

The plan is approved. The PIN question is decided: legacy peers are answered
with the device's own `bluetooth.pin`, editable and displayed, defaulting to a
value derived from the device identity rather than to a fleet-wide constant.

## ActiveForm

Bringing Bluetooth pairing to the device

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

- complete: Bluetooth subtree, reconciler, BlueZ observer, pairing agent, routes and console tab; PIN editable and displayed, identity-derived default
