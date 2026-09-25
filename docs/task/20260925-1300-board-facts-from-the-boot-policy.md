# 20260925-1300-board-facts-from-the-boot-policy The device reads its board facts from the signed boot policy, not from the board's name

- **status**: pending
- **priority**: P1
- **owner**: (unclaimed; the mica-core agent claims it)
- **createdAt**: 2026-09-25 13:00

## Description

`mica-deploy` and `mica-runkit` know four facts per board by its name: the FIT boot record geometry
(`fit_env.rs` `FitLayout`), the boot backend (`boot.rs` `BootKind::for_board`), the firmware target
and architecture (`firmware.rs`) and the kernel format and architecture (`components.rs`); the
component contract fixtures also carry a fixed board vocabulary. So a new board is a `mica-core`
change even though `mica-build` now lets a board declare its disk as data. The user asked (2026-09-25)
for the FIT record location to become configuration.

The design is `docs/plan/20260925-1300-board-facts-from-the-boot-policy.md`: a required `board`
section in the signed boot policy (`/etc/mica/boot.json` in the kernel component's initramfs, written
by `mica-build`), read by every one of those sites instead of the name.

Acceptance:

- no board name in `crates/*/src` outside tests; the four sites read `board` from the policy;
- `boot_partition` refuses a disk whose record partition differs from the policy's geometry;
- every existing test of the four sites green with the geometry supplied as policy; each new refusal
  of the plan's *Tests* has a test; a third, synthetic FIT geometry round-trips through
  `Environment::load`;
- the component contract fixtures carry the `board` section; their board vocabulary follows item 7
  of the plan;
- `make check` green; a release cut; `mica-build` told the release (it pins it and writes the section
  in one commit: `mica:docs/plan/20260921-1142-merge-boards-into-build.md`, P4).

## ActiveForm

Waiting to be claimed.

## Dependencies

- **blocked by**: (none)
- **blocks**: `mica-build`'s counterpart commit (merge plan P4 in `mica`)

## Notes

- 2026-09-25 13:00: plan written from `mica-build` (`d63ad16a`) at the user's request; measured at
  `mica-core` `1a028e3`, where `make rust-gate` passes (the baseline for this work).
