# 20260920-0542-feature-inventory The capability inventory and the deployment doc drift

- **status**: completed
- **priority**: P3
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 05:42

## Description

A read of every crate against `docs/architecture.md` and `docs/design/` found the
prose accurate everywhere but one place, and found one capability index missing.

Verified against code, not prose: the eight reconcilers `reconciler::all()`
returns, the D-Bus members `bus.rs` serves, the 50 routes in
`crates/mica-apid/openapi.json`, the settings subtrees in
`micad-settings/src/model.rs`, the `mica-deploy` subcommands, the accepted
schema ids, the board vocabulary, the mqttd topic prefixes and its two modes.
All match what the documents say.

What does not match:

- `docs/design/deployment.md` names the products `x64-dev` and `x64-minimal`.
  Both spellings are retired. The assembly builds `uefi-x64-dev`,
  `uefi-x64-prod`, `uefi-arm64-dev`, `uefi-arm64-prod`, `cx3576-dev`,
  `cx3576-prod` and `s905x5m-dev`, and the shared fixture's `product` is
  `uefi-x64-dev`. The 20260919-1030 board rename moved the code and the
  fixtures; this sentence was missed.
- The same section lists the shared fixtures as generated from `cases.json` and
  `firmware.json` and does not mention `catalog.json`, the `schemas` block or
  the canonical field order of an envelope -- the last of which
  20260920-0100 records as a wire-contract rule that no document states.

What is missing: nothing points from a capability to the document and the crate
that own it. `architecture.md` answers "how do the parts fit together" and the
design pages answer "how does this part work"; neither answers "what can this
device do, and where is that implemented".

Acceptance: `docs/features.md` exists as an index of capability to owning crate
and document with no design prose duplicated into it, `docs/README.md` lists it,
and `docs/design/deployment.md` names the current products and the protocol
contract facts.

## ActiveForm

Writing the capability inventory and correcting the deployment document

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

Documentation only; no package inputs hash moves.

- complete: docs/features.md written, deployment.md drift corrected, micad.md section order fixed
