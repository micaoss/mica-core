# 20260926-0841-one-binary-and-an-optional-console One management binary, an optional console, stripped executables

- **status**: in_progress
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-26 08:41

## Description

Shrink what a device carries for management. Every shipped executable but `mica-runkit` is
unstripped (`mica-apid` 17.3 MB, 3.9 MB of it symbol tables); the console is compiled into apid
(1.3 MB), so an API-only product cannot leave it out; and micad and apid link the same stack twice.

The design is `docs/plan/20260926-0841-one-binary-and-an-optional-console.md`: strip every
executable, move the console into a `mica-apid-ui` package apid serves from disk, and make
`mica-apid` a symlink to a multi-call `micad`, all three packages from the `micad` producer.

Acceptance: every package measured against `20260926-0815` before and after; an API-only
product is `micad` + `mica-apid` without `mica-apid-ui`, with `/_ui` answering 404 and the API
unchanged; `mica-apid --version` and `--openapi` answer through the symlink; `make rust-gate`,
the UI checks and the package gate green; a release cut and mica-build told to add
`mica-apid-ui` where a product wants the console.

## ActiveForm

Moving the console out of the API binary and folding apid into micad

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

- 2026-09-26 08:55: implemented. `make rust-gate` 1350 passed; the UI build contract, the shell
  lint and the amd64 package gate (71/71, 8 archives) pass. Measured totals are in the plan's
  annotations. Open: commit, the release, and mica-build told to add `mica-apid-ui` to every
  product that wants the console.
