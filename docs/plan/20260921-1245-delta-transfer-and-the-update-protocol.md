# 20260921-1245-delta-transfer-and-the-update-protocol Delta transfer, and the update protocol an origin must serve

- **status**: proposed
- **createdAt**: 2026-09-21 12:45
- **relatedTask**: (none yet)

## Context

Two questions were asked together: should `mica-core` become an independent
upgrade unit, and can `mica-core` and `mica-podman` be upgraded independently
of the rest of the root. The premise behind both is correct and measured below
-- the Mica-authored payload is small against the image that carries it -- but
the measurement points somewhere other than a new upgrade unit.

This record holds both halves: **Part 1 is the update protocol as the device
enforces it today**, written so that an origin can be built or corrected
against it, and **Part 2 is the delta-transfer proposal** that the measurement
argues for.

### What was measured (2026-09-21)

Published assets, read at `origin`:

| Artefact | Compressed bytes |
| --- | --- |
| `mica-uefi-x64-prod-20260920-0622.micaupd` (`full`) | 81.4 MB |
| `mica-uefi-x64-prod-20260920-0622.root.micaupd` (rootfs objects only) | 65.3 MB |
| `ghcr.io/micaoss/mica-core:pool.amd64.20260921-0726` (7 packages) | 12.8 MB |
| `ghcr.io/micaoss/mica-podman:pool.amd64.20260916-0846` (1 package) | 36.2 MB |
| `ghcr.io/micaoss/mica-system-base:pool.amd64.20260915-1102` (4 packages) | 1.3 MB |

**The pool figures and the archive figures have different provenance** -- a pool
layer is a compressed tar of `.deb` files, each itself compressed; the root is a
squashfs. The ratio is indicative and not an exact share of the image. Per
package sizes were not measured.

One result reframes the question before any proposal does: **the largest single
thing in the root is not Debian, it is podman.** `mica-core` is roughly a fifth.

### The decisive measurement: 96.8% of a new root is already on the device

Content-defined chunking (gear hash, ~16 KiB average chunk, 4 KiB/64 KiB
bounds) over two **published** `uefi-x64-prod` roots, `20260916-0845` ->
`20260920-0622`, a span in which `mica-core`, `mica-podman`,
`mica-system-base` and `mica-boards` all moved:

```
old  81.4 MB / 3908 chunks          new  65.3 MB / 3119 chunks
bytes of the new image not present in the old:  2.1 MB  (3.2%)
```

**A device that holds the previous root needs 2.1 MB of genuinely new bytes and
is sent 65.3 MB.** Stated as its limits: this is an ideal chunker; it excludes
the chunk index (3119 chunks at ~40 bytes is ~125 KB, negligible) and
per-request overhead; the old file was the `full` archive and the new one the
`root` archive, so their framing differs, which content-defined chunking is
specifically built to absorb. A real implementation does worse than 3.2% and
not by an order of magnitude. **Neither limit reaches the conclusion, because
the conclusion only needs the gap between 2.1 and 65.3.**

### So: not a new upgrade unit

| | saves | on which changes | costs |
| --- | --- | --- | --- |
| Split `mica-core` into a third signed component | at most ~20% | core-only releases | `mica/deployment/v3`, the producer, a third `verified_mount` in PID 1, the rollback algebra over three components -- and it still reboots |
| Delta transfer | ~97% | **every** release, including podman's 36 MB and the kernel | an acquisition path and an origin-side chunk store; **no schema change** |

Splitting the unit buys a fifth of one release kind for a schema break. It is
the wrong instrument for the measurement that motivates it, and this record
does not propose it. Independent upgrade of `mica-podman` fails a second way:
the process that manages containers is `micad`, inside the root it would be
updating, and the engine itself is a package in that root.

---

## Part 1. The update protocol the device enforces

This is a reader's guide to bytes that already exist. **It is not the
contract.** The contract is
`crates/mica-deploy/tests/component-contracts/` -- `cases.json`,
`catalog.json`, `deployment.json`, `envelope.json`, `firmware.json` -- which
`mica-build`'s producer already verifies against, and which any other producer
should verify against rather than against this prose. Where this section and
those files disagree, the files win and this section is wrong.

Everything below is enforced by `crates/mica-deploy/src/{catalog,components,
acquisition}.rs`. **There is no version negotiation and no compatibility
reader: a document that is not the current schema is refused, not downgraded.**

### 1.1 Transport

- HTTP/1.1 `GET`, `http` or `https`, one connection per object.
- Up to **10** redirects (`301/302/303/307/308`), because an origin may sit
  behind a CDN. A redirect target must itself be `http`/`https`.
- Resume: `Range: bytes=<offset>-`, and then **only `206` starting at exactly
  that offset is accepted**; without a range, only `200`.
- `User-Agent: mica-deploy`, `Accept: */*`.
- Connect timeout 15 s; whole-transfer ceiling 1800 s; a transfer moving less
  than 1024 bytes/s averaged over a 30 s window is abandoned.

### 1.2 The catalog source URL

Exactly one shape, checked before a byte is fetched: length <= 2048,
scheme `http` or `https`, a host, **no** userinfo, **no** query, **no**
fragment, and the path exactly `/v1/manifest.json`. Everything else about the
origin is derived from it.

### 1.3 The envelope, `mica/update-envelope/v1`

Every signed document -- the catalog and every deployment descriptor -- is
carried in this envelope:

```json
{"schema":"mica/update-envelope/v1","keyId":"<64 hex>","payload":"<base64>","signature":"<base64>"}
```

- **The four fields are in that wire order.** The device re-serialises the
  parsed envelope and requires the result to equal the received bytes; a
  producer that emits key-sorted JSON (`keyId, payload, schema, signature`) is
  refused as `noncanonical envelope`. This is the one trap that costs an
  afternoon, so the fixtures carry it explicitly as `envelopeWireOrder`.
- `keyId` is the **sha256 of the 32-byte raw Ed25519 public key**, lowercase
  hex. The trust set holds 1 to 8 keys; a `keyId` outside it is
  `untrusted metadata key`.
- `payload` and `signature` are canonical standard base64 (re-encoding must
  reproduce the string exactly). The signature is Ed25519 over the **decoded
  payload bytes**, 64 bytes exactly.
- Size ceilings: a deployment payload <= 16384 bytes and its envelope <= 24576;
  a catalog payload <= 1 MiB and its envelope <= `payload * 4 / 3 + 1024`.

### 1.4 The payload is canonical JSON

Every payload -- catalog and deployment -- is parsed, re-serialised and
required to equal the received bytes. In practice: **compact separators, no
insignificant whitespace, object keys sorted, no duplicate keys**. The envelope
is the exception above: its order is the struct order, not sorted order.

### 1.5 The catalog, `mica/catalog/v2`

```json
{"schema":"mica/catalog/v2","revision":<u64>,"issuedAt":"<RFC3339>","expiresAt":"<RFC3339>",
 "channels":[{"board":"","product":"","channel":"","releaseId":"","generation":<u64>}],
 "releases":[{"id":"","channel":"","notes":"","deployment":"<envelope string>","objects":[{"sha256":"","bytes":<u64>,"url":""}]}]}
```

Unknown fields are refused at every level (`deny_unknown_fields`).

- `revision` > 0 and <= 2^53-1; **<= 128 releases; <= 12 channel heads**.
- `issuedAt` <= now + 300 s, `expiresAt` > now, `expiresAt` > `issuedAt`, and
  **the validity window is at most 30 days**.
- Freshness across fetches, against the device's stored checkpoint for the
  **same source URL**: the revision may not go backwards, and **the same
  revision must carry byte-identical signed contents** (compared by payload
  digest). Re-signing the same revision with different contents is refused.
- `channel` is one of `stable`, `beta`, `dev`, both in a release and in the
  device's request.
- Release `id`: non-empty, <= 128 bytes, unique within the catalog.
  `notes` <= 10000 characters.
- `deployment` is the **full signed envelope string**, verified independently
  of the catalog's own signature.
- `objects` must name **exactly** the distinct artefacts of that deployment --
  no more, no fewer -- each with a matching `bytes`; a digest that is not one
  of them is `component object substitution`.
- **Every object URL must equal `<origin>/v1/objects/<sha256>`** derived from
  the catalog source. An object served from a second host, or under a name that
  is not its digest, is refused. A CDN is reached through a redirect, not
  through a different URL in the catalog.
- `(board, product, channel, generation)` is unique across releases.
- `channels` must be **exactly** the set of highest-generation releases, one
  per `(board, product, channel)` present in `releases`: same count, and each
  head naming that group's highest `generation` and its `releaseId`. A head
  that lags its own releases is refused.

### 1.6 Selection

A release is a candidate when `board`, `arch` and `product` equal the device's
and `channel` equals the requested one, and its `generation` is **strictly
greater than the device's `highestGeneration` floor**; the highest such
generation wins. The floor is monotonic, so a re-published lower generation is
not an update. A catalog with no candidate is a valid catalog and a normal "no
update" answer.

### 1.7 The deployment descriptor, `mica/deployment/v2`

```json
{"arch":"","board":"","dataPolicy":"unchanged","generation":<u64>,
 "kernel":{"arch":"","board":"","boot":{"artifact":{"bytes":<u64>,"sha256":""},"format":"uki|fit"},
           "buildId":"<64 hex>","id":"<64 hex>","release":"","schema":"mica/kernel/v1",
           "support":{"image":{...},"rootHash":"<64 hex>","signature":{...},"verity":{...}}},
 "product":"","rootfs":{"arch":"","content":{"image":{...},"rootHash":"","signature":{...},"verity":{...}},
 "id":"<64 hex>","schema":"mica/rootfs/v2"},"schema":"mica/deployment/v2","version":""}
```

- `dataPolicy` is `unchanged` and nothing else.
- **The board decides the architecture and the boot format**, and the set is
  closed: `uefi-x64` -> `amd64`/`uki`, `uefi-arm64` -> `arm64`/`uki`,
  `cx3576` and `s905x5m` -> `arm64`/`fit`. Any other board is
  `unsupported board`. (The generic systems were renamed on 2026-09-16; `x64`
  and `virt-arm64` are refused, with no alias.)
- `product` must equal the device's own: the single unquoted `PRODUCT=<name>`
  line of `/usr/lib/mica/product.conf`. **The origin is keyed by product**, and
  a descriptor for another product of the same board is refused.
- `version` and `product` follow the identifier rule: 1..=128 bytes, first
  character alphanumeric, thereafter alphanumeric or `.` `_` `+` `-`.
- `generation` > 0.
- Component schemas are pinned: `mica/kernel/v1` and `mica/rootfs/v2`.
- **Component ids are derived, not chosen**: `id` is the sha256 of that
  component object serialised canonically **with its own `id` field removed**.
  A descriptor whose `kernel.id` or `rootfs.id` does not reproduce is
  `component identity mismatch`. This is why an origin cannot relabel a
  component.
- Verity geometry is fixed: version 1, `sha256`, 4096-byte data and hash
  blocks, `hashOffset == dataBlocks * 4096`, and the image length must equal
  `hashOffset + <tree blocks> * 4096` computed with a fan-out of 128. A
  signature artefact is <= 65536 bytes.
- At boot, `mica-runkit` additionally binds the descriptor to the running
  kernel: `board`, `arch`, `kernel.buildId`, `kernel.release` and the derived
  support id must equal what is running.

### 1.8 Objects

The five artefacts of a deployment, addressed only by digest:

`kernel.boot.artifact`, `kernel.support.image`, `kernel.support.signature`,
`rootfs.content.image`, `rootfs.content.signature`.

The same digest may appear twice only with the same length. Downloads land as
`downloads/<sha256>.partial`, are verified against digest **and** length, then
`rename`d into the object store with the directory synced. **A failed
verification deletes the partial file**; nothing unverified is ever promoted.

Installed paths are derived from the ids, never from a name in the document:
`roots/<rootfsId>/rootfs.img`, `kernels/<kernelId>/support.img`, and
`EFI/mica/kernels/<kernelId>.efi` (UKI) or `kernels/<kernelId>/boot.itb` (FIT).

### 1.9 The offline archive, `MICAUPD1`

```
"MICAUPD1"            8 bytes
descriptor length     u32 big-endian, 1..=24576
descriptor            that many bytes: the signed envelope
object count          u32 big-endian, <= the number of objects the descriptor names
per object:           sha256 as 64 ASCII characters
                      size as u64 big-endian
                      exactly that many bytes
```

No filenames, no directory entries, no links, no compression, no padding.
**An archive may carry any subset of the descriptor's objects, including
none**; every object it does not carry must already be in the device's store.
This is what makes `root`-only and `kernel`-only archives possible: they are
the same signed descriptor with fewer objects. An object that the descriptor
does not name, a duplicate, or a wrong length is refused.

### 1.10 Where the refusals are enumerated

`cases.json` carries every refusal with the rule it fires, in both directions:
a producer can drive its own output through them, and `contract_rule_coverage`
in this repository fails if a rule in `components.rs` has no negative case.
**An origin that passes those cases is an origin this device will talk to**;
prose agreement with this section proves nothing.

---

## Part 2. Delta transfer

### 2.1 What does not change

**No schema changes.** Not the envelope, not `mica/catalog/v2`, not
`mica/deployment/v2`. The descriptor still names each object by digest and
length; the catalog still names `<origin>/v1/objects/<sha256>`. A delta is a
*transport* for an object whose identity is already signed, and the device's
existing `promote()` already refuses anything whose digest does not match.

**Therefore the index and the chunks need no signature.** The worst a hostile
or corrupt index can do is waste bandwidth: the reconstructed object is
verified against the signed digest and deleted on mismatch. That property is
what keeps this proposal small, and it should be stated in the design rather
than discovered.

### 2.2 The shape

For each object the device is missing:

1. `GET <origin>/v1/objects/<sha256>.index`. A `404` is a normal answer: the
   device falls back to the full object, which is the path that exists today.
2. The index lists the object's chunks in order: `(sha256, length)`.
3. The device builds a local chunk map by **re-chunking the sources it already
   holds** with the same algorithm -- principally the installed root and
   support images under `/system`, which are already verity-verified, plus any
   objects still in its own store.
4. Chunks not found locally are fetched from
   `<origin>/v1/chunks/<sha256>`, each verified against the index entry.
5. The object is assembled in `downloads/`, then goes through the **existing**
   verification and promotion. Any failure at any step falls back to the full
   object.

**The chunker must be pinned exactly, because both sides run it.** The device
re-chunks its local seed; the origin chunks what it publishes; the boundaries
must agree or reuse collapses to zero. The parameters (hash table, mask,
minimum, maximum) belong in a vector under
`crates/mica-deploy/tests/component-contracts/`, with a fixed input and its
expected chunk boundaries, so that a second implementation is checked rather
than described.

**Content-defined, not fixed-size.** squashfs lays its compressed blocks end to
end; one block changing length shifts every byte after it. Fixed-size chunking
would turn the measured 96.8% into single digits. This is the single most
likely way to implement this proposal and get nothing for it.

### 2.3 The origin's side also gets smaller

The chunk store is content addressed and shared across releases, so publishing
the next release adds only the chunks that are new -- **about 2 MB per release
rather than 65 MB**, by the same measurement. Storage is O(total distinct
content), not O(releases), and not O(release pairs).

### 2.4 The alternative, and why it is second

A pairwise delta (`/v1/deltas/<from>-<to>`, one file, apply and verify) is
simpler on the device -- no local chunker, no chunk map -- and captures the
same bytes for a device exactly one release behind. It loses on two counts: the
origin must publish a delta per (from, to) pair it wants to serve, and a device
several releases behind, or on a different channel, falls back to the full
object. It is the right answer only if the chunk store is unwanted; it is
recorded here so the choice is visible rather than implied.

### 2.5 Cost and non-goals

- **Disk is unchanged.** Reconstruction writes the same object into the same
  staging directory; the installer's existing capacity preflight already covers
  it. This proposal changes bytes on the wire, not bytes on DATA.
- **CPU**: hashing one 65 MB seed on the device, once per object, before any
  network transfer. Measured on the device, not estimated here.
- **Not proposed**: any change to A/B, rollback, generation floors, trust, or
  the reboot. A delta makes the same deployment cheaper to fetch; it does not
  make it a different deployment.

### 2.6 Acceptance

1. A vector file pins the chunker and a second implementation reproduces its
   boundaries.
2. An origin serving `.index` and `/v1/chunks/` is driven end to end from a
   device holding the previous root, and the transferred bytes are **measured**
   -- the number to beat is 65.3 MB, and the modelled floor is 2.1 MB.
3. A missing, truncated, corrupt or hostile index falls back to the full
   object, and the object that lands is byte-identical either way.
4. `cases.json` gains the index negatives, so the fallback is a tested path
   rather than an asserted one.
5. The whole feature is absent from the signed documents: a catalog produced
   before this work is still valid, and an origin that never publishes an index
   is still a conforming origin.

---

## Note on the current origin implementation

Read 2026-09-21 in the `mica-fleet` checkout (`f048a4ae`), and recorded here
because it decides what "conforming" means today rather than to assign work:
`apps/updates` is written against `mica/catalog/v1`, `mica/deployment/v1` and
`mica/rootfs/v1` (`src/catalog.ts`, `src/deployment.ts`, `src/blobs.ts`,
`src/bun/blobs.ts`, `test/helpers.ts`). Those three schemas were superseded on
2026-09-15 (`mica:docs/decisions/2026-09-15-update-packages.md`); its
`mica/kernel/v1`, `mica/firmware/v1` and `mica/update-envelope/v1` are current.
**The device reads only the current schemas and refuses the rest**, so that
origin cannot serve a Mica device until those three move to v2. Part 1 above,
and the contract fixtures it points at, are what it must satisfy.
