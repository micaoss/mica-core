# 20260920-0900-pinned-vectors Read the release-lock vectors out of mica at a pinned commit

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 09:00

## Description

`tests/vectors/` is a **snapshot**, not a subset: byte-identical to mica's
canonical `docs/design/release-lock/vectors/expected.tsv` at commit `4df34ee`
(2026-09-15 02:34:28Z), 51 vector rows against the canonical 84, with nine mica
commits since. It passes 51 of 51 and reports green, which is a conformance test
conforming to itself.

Authorised 2026-09-20 (coordinator, spec section 9.1): **the vectors are not
copied — a consumer reads them out of `mica` at a pinned commit and refuses a
difference**, the mechanism `mica-build:tools/deploy-pool.sh --check` already
uses for this repository's component fixtures.

**The direction is the part worth naming.** `crates/mica-deploy/tests/component-contracts/`
is bytes **this repository produces and another verifies**; the release-lock
vectors are bytes **another repository produces and this one must verify**. Same
mechanism, opposite direction — and every stale copy found on 2026-09-20 was the
second direction: a consumer holding a private copy of somebody else's truth.
That is why the first direction was built first and the second was missing
everywhere.

## What this round does

1. **`data` rows in the reader.** `check-lock.sh` refuses `data` with
   `kind-unknown` today. The required subset is derived from `locks/pins/`, and
   any pinned producer may carry producer data (spec 1.2.4), so the kind is in
   scope even though mica-build-env publishes none today:
   `data <name> <file> <sha256>`, `<name>` the key, `<name>` and `<file>`
   matching `[a-z0-9][a-z0-9.+-]*`, sorted within the kind.
2. **The pinned read.** Fetch `mica` at a pinned commit into the git-ignored
   source cache, run the reader against the vectors there, and delete the copy.
3. **Set equality, both directions** (mica-boards' requirement): every canonical
   row is accounted for and nothing runs that the canonical does not name. A
   mechanism that only fails on absent vectors leaves a stale extra in place
   forever -- mica-boards still carries a fixture for a board that no longer
   exists, green because the fixture and the reader agree with each other.
4. **The required subset derived, not declared** (mica-podman's requirement):
   from `locks/pins/` plus the forms this repository **produces** and checks in
   its own lock. Today that is the unscoped release row, `image`, `source`,
   `git`, `pool`, `package`, `data`, and the pins in `ci` and `local` modes.
   Not in scope: `index`, board component, `bundle`, `asset` and `update` rows,
   because nothing here reads a lock that may carry them.
   **And the negatives belong to the subset too**: `scoped-release-not-allowed`,
   `pins/refused/scope-not-allowed` and `release-slash` are the vectors asserting
   what *this* repository's forms may not be. A subset derived only from what a
   reader consumes would miss them, which is the trap in deriving from pins
   alone.

## What landed

`scripts/gate/vectors-source.sh` reads the pin, fetches `mica` at that commit
into the git-ignored `repos/` cache and verifies `HEAD` is the pinned commit --
git refuses an object that does not hash to its name, **so the checkout is the
pin**. A warm cache needs no network, so `make offline` keeps working: this
repository keeps **no copy and still runs offline**, which answers the trade the
coordinator had assumed bound. A copy in the tree can drift silently; a cache
cannot, because its directory name is the commit and a mismatched `HEAD` is a
refusal.

`make locks-test`: **68 passed, 0 failed, 88 rows read, 35 outside the floor**,
with both directions of the comparison: every vector file in the pinned tree is
named by its manifest, and every manifest row is run rather than skipped.

- **The `data` kind**, all six vectors correct. `data-file` needed what the spec
  does not say in the word it uses: the **file** is a second key even though the
  **name** is called the key, because two rows naming one asset leave a consumer
  no way to say which it fetched. The first implementation passed that vector as
  valid.
- **The `vectors-pin` mode**, all seven vectors correct, and the gate validates
  this repository's own `scripts/gate/vectors.pin` through it. That family did
  not exist when this round started: the pin was written, the commit pinned, the
  vectors read at it, and they refused the new file for missing its header --
  within the hour, with nobody reviewing it.
- **`board` and `apt` are deleted from the reader.** They were implemented from
  an older spec and no lock here carries either. A stale implementation answered
  `column-count`, *a claim about the row's shape*, where `kind-unknown` is the
  honest answer. A wrong confident answer is worse than an absent one.
- **The floor is derived from content, not from file names.** A vector is
  outside it when its release row carries a scope or it holds a kind this reader
  does not implement, both read out of the vector. `release-slash` was in the
  first floor list **because of its name**: it is a `mica-boards` lock with
  `uefi-x64/20260914-2042`, a scoped form this repository neither pins nor
  emits. A name did the work a measurement should have done, in a list handed to
  three other repositories.
- **`tests/vectors/` is deleted**, which is also the repair of the hand-edited
  `other-kind.lock`: a blob that is not in the tree cannot be edited.

## Still open

The scope rules (`release-scope`, `scope-content`) and the `index`, `board`
component, `bundle`, `asset` and `update` kinds are unimplemented and their 35
vectors are reported as outside the floor on every run. Implementing them is
not required by the derivation and would be conformance nobody here needs --
but the report is what keeps that a decision rather than an omission.

## The pin format question, answered by the coordinator

**Where the pin lives for an input that has no releases.** Every existing pin
names a release: `locks/pins/<repository>.pin` is `mica-pin v1` with
`RELEASE=` and a `SHA256SUMS` trust hash, and `mica-build:tools/source.sh`
derives its commit from a lock's release row. `mica` publishes no releases, so
neither shape fits, and all four repositories need the same answer. Proposed
here and implemented as a candidate rather than settled:
`scripts/gate/vectors.pin`, outside the spec-defined `locks/pins/` so it cannot
be mistaken for a producer pin, naming `REPOSITORY=mica` and the full 40-hex
`COMMIT=`.

## ActiveForm

Reading the release-lock vectors at a pinned commit instead of copying them

## Dependencies

- The pin format for a release-less input is a cross-repository decision; the
  candidate above is local until the coordinator rules.
