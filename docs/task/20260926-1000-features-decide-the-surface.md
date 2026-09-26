# 20260926-1000-features-decide-the-surface The product's features decide which API a device serves

- **status**: in_progress
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-26 10:00

## Description

Every device serves every route and runs every reconciler whatever the product carries, and a
board without a radio, SSH, containers or MQTT answers those routes with errors. The product's
`FEATURES` in `/usr/lib/mica/product.conf` (dm-verity root, written by the assembly) is to decide
what micad runs, what apid serves and what the console shows.

The design is `docs/plan/20260926-1000-features-decide-the-surface.md`.

Acceptance: with `FEATURES` lacking `wifi`, `bluetooth`, `ssh`, `containers` or `mqtt`, that
feature's reconciler is not registered, its settings writes are refused, its bus members answer
one named refusal, its routes answer 404, `/api/v1/meta` omits it and the console shows no pane
for it; a development root without `product.conf` serves everything; the gates green; a release,
with mica-build told to add `ssh` to the products that carry SSH.

## ActiveForm

Letting the product's features decide the API surface

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

- 2026-09-26 10:40: implemented. `make rust-gate` 1359 passed; UI 283 passed; shell lint and UI
  build contract pass. Open: commit, the release, and mica-build told to list `ssh` in `FEATURES`
  for the products that carry `mica-ssh`.
