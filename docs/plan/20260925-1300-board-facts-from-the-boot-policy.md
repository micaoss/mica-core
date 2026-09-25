# 20260925-1300-board-facts-from-the-boot-policy The device reads its board facts from the signed boot policy, not from the board's name

- **status**: approved
- **createdAt**: 2026-09-25 13:00
- **approvedAt**: 2026-09-25 13:00 (user: "mica-deploy 在设备端按板卡名写死了 FIT 引导记录的位置，这个也需要修复，可以引入配置文件形式", then "你写一个计划给core我让core去执行"; the carrier and the scope take the recommended answers below)
- **relatedTask**: 20260925-1300-board-facts-from-the-boot-policy
- **counterpart**: `mica:docs/plan/20260921-1142-merge-boards-into-build.md`, P4 (the `mica-build` half)

## Context

Measured at `mica-core` `1a028e3`, `mica-build` `d63ad16a`.

`mica-build` now lets a board declare its disk (`boards/<board>/layout.tsv`: partitions, roles, raw
regions) and builds and verifies the image from it; a new board that reuses an existing boot backend
is data there. The device side still knows four facts per board **by name**, so a new board is a
`mica-core` change:

| Site | What it keys on the board name |
|---|---|
| `crates/mica-deploy/src/fit_env.rs` `FitLayout::{Cx3576, S905x5m}`, `for_board` | the two boot record offsets inside the firmware partition, and its sector count |
| `crates/mica-deploy/src/boot.rs` `BootKind::for_board` | UEFI or U-Boot FIT |
| `crates/mica-deploy/src/firmware.rs` `parse_firmware`, `verify_installed` | the architecture of each board, the EFI loader name, the exact rockchip write range (`cx3576`: 32768 / 16744448) and Amlogic payload (`s905x5m`: 512 / 4193792), and which `FitLayout` a target belongs with |
| `crates/mica-deploy/src/components.rs` `Deployment::validate` | the architecture and kernel format (`uki` / `fit`) of each board |

Their callers: `bin/mica-deploy.rs` (`execute`: `BootKind`, `FitLayout`, `boot_partition`;
`authenticate_firmware`), `bin/mica-runkit/init.rs` (`BootKind` at line ~337, `boot_partition`,
the `Operation::Retire` it constructs with the board name), `boot/shutdown.rs` (`Operation::Retire`
re-derives `BootKind` and `FitLayout` from the name), `deployments.rs` `boot_partition` (checks the
FIRMWARE partition's start 64 and `FitLayout::sectors()`).

`fit_env.rs` says why the geometry is compiled in: "disk contents never choose writable offsets".
That rule stands: the replacement must be authenticated data, not something read off the disk.

## Proposal

**The carrier: the signed boot policy.** The kernel component's initramfs carries
`/etc/mica/boot.json` (`mica-build:src/image/kernel-package.ts` writes it; `mica-runkit` reads it at
`init.rs` and copies it to `/run/mica/boot-policy.json`, which `mica-deploy` reads). It is inside the
signed UKI / FIT, board-specific, and present in the initramfs, where two of the consumers run
(`runkit` choosing the backend; `Retire` in startup PID1) and the root is not mounted yet. A file in
the dm-verity root was considered and rejected for that reason: the initramfs consumers cannot read
it. `Config` (runkit) and `Policy` (mica-deploy) both `deny_unknown_fields`, which is what makes the
rollout order below matter.

**The schema.** `boot.json` gains one required object, `board`, beside `identity`, `publicKeys`,
`systemPartUuid`, `dataPartUuid` (canonical JSON, keys sorted, as today):

```json
"board": {
  "boot": "uboot-fit",
  "kernel": "fit",
  "firmware": { "format": "rockchip-loader", "diskOffset": 32768, "maxBytes": 16744448 },
  "records": { "partition": 1, "startSector": 64, "sectors": 36800, "offsets": [16744448, 17793024], "size": 65536 }
}
```

- `boot`: `uefi` or `uboot-fit`, the device's existing `BootKind` serialization (the boot receipt
  and the contract fixtures already spell it so; `mica-build` maps its `systemd-boot` onto `uefi`).
  Replaces `BootKind::for_board`; `BootKind` itself stays, deserialized from this value.
- `kernel`: `uki` or `fit`, the kernel component's boot format. Must agree with `boot`
  (`uefi` ⇔ `uki`, `uboot-fit` ⇔ `fit`).
- `firmware`: exactly the `target` object a firmware receipt for this board carries
  (`mica-build:src/image/firmware-formats.ts` `target(facts)`): `{format: efi, partition: 1, path}`,
  `{format: rockchip-loader, diskOffset, maxBytes}` or `{format: amlogic-boot0, payloadOffset,
  maxBytes}`. `efi` ⇔ `uefi`.
- `records`: present exactly when `boot` is `uboot-fit`. The GPT number, start sector and sector
  count of the partition that holds the two boot record copies, their offsets **inside that
  partition** (the convention `fit_env.rs` already uses) and their size, which must be `ENV_SIZE`
  (65536). Today's values: cx3576 `{1, 64, 36800, [16744448, 17793024], 65536}`, s905x5m
  `{1, 64, 262080, [125796352, 129990656], 65536}`.

The architecture is already `identity.arch`; it is not repeated.

**What each site becomes.**

1. `fit_env.rs`: `FitLayout` stops being an enum and `for_board` goes. It becomes a small struct
   built from `board.records` with a validating constructor: size == `ENV_SIZE`; both copies inside
   `sectors * 512`, non-overlapping; offsets and sizes bounded. `offsets()` and `sectors()` keep
   their meaning, so `Environment::load` and its callers change only in how they obtain the value.
2. `boot.rs`: `BootKind::for_board` goes; `BootKind` is parsed from `board.boot`.
3. `deployments.rs` `boot_partition`: takes the `FitLayout` (or the records object) instead of the
   board name and holds the actual partition to it -- partition number, `start` == `startSector`,
   `size` == `sectors` -- refusing on any difference ("FIRMWARE geometry differs from the signed boot
   policy"). This is the fail-closed guard for a policy that names a geometry the disk does not have
   (a disk's geometry is fixed at the factory; a later kernel carrying another one must not write).
4. `firmware.rs`: `parse_firmware` keeps the shape and bound checks it has per format, and loses the
   per-board tables (the architecture per board, `firmware.board == "s905x5m" && ...`, the EFI loader
   name per board). In their place, the device checks the manifest against the policy: `firmware.arch
   == identity.arch`, `firmware.board == identity.board`, and `firmware.target` equal to
   `board.firmware` (canonical comparison). Where `parse_firmware` has no policy at hand today, pass it
   in (the call sites are `bin/mica-deploy.rs` and `verify_installed`). `verify_installed` matches on
   the target format alone, not on a `FitLayout` variant.
5. `components.rs` `Deployment::validate`: the architecture and kernel format come from the policy
   (`identity.arch`, `board.kernel`) instead of the board table. `verify_deployment` already receives
   the `BootIdentity`; give it the board section too, or check the two facts at its callers --
   whichever keeps `parse_deployment` usable where a deployment is read without a boot policy (the
   acquisition path in `acquisition.rs`: there the board and arch are compared with the running
   policy's anyway, so the format check can move to the same place).
6. `Operation::Retire` (runkit constructs it, `shutdown.rs` executes it): carries the backend and the
   records object from the policy the initramfs read, instead of the board name it re-derives both
   from.
7. The board vocabulary of the component contract fixtures
   (`crates/mica-deploy/tests/component-contracts/cases.json`, `boards`), which `mica-build`'s
   `deploy-pool --check` holds equal to its `boards/boards.tsv`: once no code keys on a board name,
   a fixed board list is a fifth place a new board touches `mica-core`. The fixtures keep their
   concrete boards as cases, and the vocabulary either goes or becomes the name *pattern* a board
   must match; `mica-build` changes its check in the same counterpart commit.

After this, `grep -rE '"(cx3576|s905x5m|uefi-x64|uefi-arm64)"' crates --include='*.rs'` outside
tests finds nothing.

**Tests.** Every existing test of the four sites keeps its meaning with the geometry supplied as
policy (the FIT fixtures in `crates/mica-deploy/tests/{fit_env,fit_deployments,io_faults,firmware}.rs`
build `FitLayout` from the two boards' records); new refusals, each by name: a missing `board`, an
unknown `boot` or `kernel`, `boot`/`kernel`/`firmware.format` disagreeing, `records` on a UEFI
policy or absent on a FIT one, a record copy outside its partition, two copies overlapping, a size
other than 65536, a partition whose start or size differs from the policy, a firmware receipt whose
target differs from `board.firmware`. A synthetic third FIT geometry (not cx3576's, not s905x5m's)
is written and read back through `Environment::load` to show a new board needs no code. The
component contract fixtures (`crates/mica-deploy/tests/component-contracts`, which `mica-build`'s
`deploy-pool --check` compares at the pinned release) gain the `board` section.

**Rollout, and who does what.** The two halves cannot land in either order, because `deny_unknown_fields`
refuses an unknown key and the new code requires the key:

1. `mica-core` (this plan): implement, `make check` green, release. Nothing changes on a device until
   `mica-build` pins the release.
2. `mica-build` (the counterpart, owned there): in **one commit**, pin that release
   (`locks/mica-core.lock` and its pin), write `board` into `boot.json` from the board's `layout.tsv`
   and firmware facts, follow the contract fixtures' board vocabulary (item 7), delete `src/image/device-fit-geometry.ts` and the layout rule that refuses a
   FIT board the device does not know, and add the section to the verifier's reading of `boot.json`.
   Its proof: the four products build and verify in CI; the kernel components change by exactly the
   new section.

A deployment always pairs its root with the kernel built beside it, so a device never runs a new
`mica-deploy` against an old kernel's policy; a rollback boots the old pair. No compatibility path
for a policy without `board` is kept (development phase, as elsewhere in this tree).

## Risks

- **A policy that names another geometry than the disk has.** Guarded by `boot_partition`'s check
  against the live GPT (item 3); `mica-build` also refuses to change a released board's record
  geometry without a new board (its layout rules).
- **The early loader.** The boards' U-Boot (`mica-build:boards/<board>/loader/mica-file-boot.c`)
  compiles its record offsets per board; that is the board's own build and stays so. The policy and
  the loader must agree, which `mica-build` holds (both come from the same board directory).
- **Two repositories, one contract.** The schema above is the contract; the component contract
  fixtures carry it on both sides.

## Scope

In: `crates/mica-deploy` (the four sites, their callers, their tests), the component contract
fixtures and their board vocabulary, `docs/design/deployment.md`. Out: `mica-build` (its counterpart), the boards' loaders, the
update archive format.

## Progress

## Annotations
