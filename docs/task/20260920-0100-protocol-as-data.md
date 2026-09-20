# 20260920-0100-protocol-as-data The catalog vector and the schema vocabulary

- **status**: in_progress
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 01:00

## Description

A second update server, `micaoss/mica-fleet` `apps/updates`, was written on the
superseded protocol: `mica/catalog/v1`, `mica/deployment/v1`, `mica/rootfs/v1`
and the pre-rename board vocabulary. It landed on 2026-09-16 (`a3a94db9`), the
day after the user accepted the v2 moves
(`mica:docs/decisions/2026-09-15-update-packages.md`, which names "the
update-server with catalog v2 and import" as part of the delivered work) and the
day the boards were renamed. mica-build's own `update-server/` is v2 and already
uses `uefi-x64`, so the authority question is settled by record, not by argument.

Nothing was reaching a device: mica-build read the manifest out of the published
`mica-cx3576-dev-20260919-2356.img.gz` and it names **no origin at all**
(`update.source` null, `fleet.enabled` false, `fleet.url` null). An origin is
runtime configuration. So the divergence is real and not urgent, and the day
someone configures an origin that speaks v1 the client refuses it at the schema
check -- loudly, which is the correct failure.

What this repository can do about it is make the protocol **bytes both sides
verify** instead of strings each side spells from memory, the way the board
vocabulary now works.

## What changed

- `tests/component-contracts/catalog.json`, a fifth shared fixture from the same
  test-only generator: one signed `mica/catalog/v2` document, source
  `https://updates.example/v1/manifest.json`, one channel head
  (`uefi-x64`, `uefi-x64-dev`, `stable`), one release carrying the shared
  deployment envelope verbatim and the four deduped objects at
  `<origin>/v1/objects/<sha256>`. `contract_fixtures.rs` asserts it stays the
  generator's fixed point.
- `cases.json` gains a `schemas` block: the accepted strings and the refused
  ones, with `mica/catalog/v1`, `mica/deployment/v1` and `mica/rootfs/v1` named
  explicitly because a second server implements them today.
- `tests/contract_protocol.rs` verifies the vector through `verify_catalog`
  (selection, the golden `deploymentId`, origin-and-digest object URLs, and that
  another product is offered nothing), and drives every refused spelling through
  `parse_deployment`, `authenticate_deployment` and `verify_catalog`, re-signing
  with the fixture key so the schema is the only thing wrong.

## What building the vector caught

The first version put the inner deployment envelope into the catalog by
re-serialising a `Value`, which orders the four envelope fields alphabetically;
the reader refused it as `noncanonical envelope`. **Field order is part of the
wire contract and no document says so.** A server written from the design pages
makes that mistake; a server written against these bytes cannot. It is the first
thing a `mica-fleet` migration would have hit, so the vector paid for itself
before anyone took it.

**It is the difference between a specification and a contract: a specification
describes what to produce, a contract is a thing you can fail.**

## What the guard caught, again

The fixtures are test files and excluded from the inputs hash, so the claim
"nothing moves" looked safe -- and it was wrong. The regeneration example,
`crates/mica-deploy/examples/component-contract-fixtures.rs`, had to gain the
fifth file, and `crates/*/examples/**` was **not** excluded. Both packages built
from this crate moved, and the guard refused them at `0.1.0-2`. Examples are now
excluded for the same reason tests are -- a producer compiles its binaries with
`cargo build --bin`, which never builds an example -- and `mica-deploy` and
`mica-lifecycle` are `0.1.0-3`, which is what narrowing the manifest costs once.

## What the second implementation found (2026-09-20)

mica-build built a second reader for the vector rather than trusting the byte
comparison, and the sentence behind that is worth keeping:
`deploy-pool.sh --check` **proves both repositories hold the same bytes; it
cannot prove both read them the same way, and the protocol is exactly where two
implementations drift.** It found two things:

1. **The stored envelope is tidy and the wire form is not.** A `Value`
   re-serialises the four fields alphabetically; the reader re-serialises and
   compares against the bytes it was handed, so only `schema, keyId, payload,
   signature` authenticates. A consumer must rebuild that order before feeding
   the fixture to a reader -- which this repository already knew, because the
   same trap produced a `noncanonical envelope` refusal while the vector was
   being built, and `tests/components.rs` has carried an `ordered_envelope`
   helper for it. **Knowing it and not saying it in the bytes is what made the
   next consumer debug it.** `envelope.json` and `catalog.json` now carry an
   `envelopeWireOrder` field saying so, so the warning arrives with the file
   rather than in a document somebody may not read.
2. **A tampered signature is refused**, which mica-build called "the half a
   vector usually forgets to carry". It was already there, and it is what makes
   the positive case mean anything.

## Not done, deliberately

The reader accepts v2 only. No v1 acceptance, no aliases, no transition path:
the no-compatibility rule applies and there is no user instruction otherwise. If
`mica-fleet` must move, that is `mica-fleet` moving.

## ActiveForm

Turning the update protocol into bytes both sides verify

## Dependencies

- The fixtures reach the assembly only through a release pin; these files are
  test-only and excluded from the inputs hash, so no package moves.
- Only mica-build can make a server verify them, in its `update-server` tests,
  when the coordinator routes it. `mica-fleet` has no issue in this project.
