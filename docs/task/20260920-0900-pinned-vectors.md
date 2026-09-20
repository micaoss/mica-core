# 20260920-0900-pinned-vectors Read the release-lock vectors out of mica at a pinned commit

- **status**: in_progress
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

## Open question, raised with the coordinator

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
