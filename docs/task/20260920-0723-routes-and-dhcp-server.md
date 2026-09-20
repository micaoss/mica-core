# 20260920-0723-routes-and-dhcp-server Static routes and a DHCP server on a declared interface

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 07:23

## Description

Device testing asked for static routes and for a DHCP server bound to an
interface. Neither existed: `StaticConfig` carried an address, an optional
gateway and DNS servers, and the only DHCP server on the device was the one the
`wifi_ap` reconciler renders for the access point.

## What changed

`IfaceSettings` gains two optional fields, both skipped when empty, so an entry
that declares neither serializes exactly as it did before they existed -- a
document written by this build is byte-identical to one written by the previous
build until someone uses them. `network.json` keeps schema version 1 for that
reason: bumping it would make every older micad refuse every newer document,
including the ones that use nothing new, and that is the A/B rollback this
repository keeps survivable.

- `routes`: destination, optional next hop, optional metric. Rendered as
  networkd `[Route]` sections.
- `dhcpServer`: pool offset, pool size, announced DNS, lease seconds. Rendered
  as `DHCPServer=yes` plus a `[DHCPServer]` section. The pool is an offset and
  a count because that is what networkd takes -- a range converted twice is a
  range that can disagree with itself.

**A bridge port renders neither.** A port's addressing is the bridge's, so a
route or a server on one is a configuration networkd would accept and nothing
could use; the renderer drops both for a port, so a document hand-edited past
apid still produces a unit that means what the bridge means.

apid refuses, before anything is written: a destination that is not a network,
a next hop that is not an address, a second default route beside
`static.gateway`, a server on a link that is itself a DHCP client, an empty
pool, and an announced DNS server that is not an address.

The interface page edits both with the addressing they depend on, in one save:
three writes would leave a device addressed for a subnet it is not yet serving.
The server fieldset appears only where the device would accept one.

## ActiveForm

Adding static routes and a DHCP server to a declared interface

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

Verified: `cargo test -p micad -p micad-settings --locked` green (530 + 80 + 62
and the integration suites), `cargo test -p mica-apid --locked` 359 passing,
OpenAPI regenerated -- the documented `NetworkInterface` is held field-for-field
against the model by `the_network_schema_matches_the_settings_model`, which is
what made the two new schemas mandatory rather than optional --
`crates/mica-apid/ui/run.sh` green with 251 tests.

`micad` moves for the first time this cycle, so `pkgs/micad/producer.env` goes
to `0.1.0-2`; `mica-apid` is already `0.1.0-3`.

- complete: model, renderer, refusals and UI; suites green
