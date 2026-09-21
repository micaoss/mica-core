# 20260921-1300-delta-transfer A new root is 2 MB of new bytes and 65 MB of transfer

- **status**: in_progress
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-21 13:00

## Description

Measured 2026-09-21 over two **published** `uefi-x64-prod` roots,
`20260916-0845` -> `20260920-0622`, a span in which `mica-core`,
`mica-podman`, `mica-system-base` and `mica-boards` all moved: **96.8% of the
new image is already on a device that holds the old one**. The device is sent
65.3 MB to deliver 2.1 MB of content it does not have.

The question that started this was whether `mica-core` should become an
independent upgrade unit. It should not: splitting it out saves at most a fifth
of the transfer, and only on core-only releases, for the price of
`mica/deployment/v3`, a third `verified_mount` in PID 1 and a rollback algebra
over three components. The measurement points at the transport, not at the
unit. The reasoning is in
`docs/plan/20260921-1245-delta-transfer-and-the-update-protocol.md`.

## ActiveForm

Implementing delta transfer and pinning the chunker.

## Acceptance

- A chunk the device already holds is not fetched; a chunk it does not hold is
  fetched from the origin's chunk store and checked against the index.
- A missing, truncated, hostile or substituted index or chunk falls back to the
  whole object, and the object that lands is identical either way.
- The chunker is pinned by a vector a second implementation can reproduce from
  the derivation alone, index bytes included.
- No signed document changes: an origin that publishes no index is still a
  conforming origin.

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

- The plan's Part 1 writes down the whole update protocol the device enforces,
  so that an origin can be built or corrected against it. It is a guide to the
  contract fixtures, not a second contract.
- `crates/mica-deploy/tests/component-contracts/` gains `chunker.json`.
  `mica-build` copies that directory and diffs it against this repository at
  the commit of its pinned release, so the file arrives there when the pin
  moves, not before.
