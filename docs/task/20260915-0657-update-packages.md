# 20260915-0657-update-packages Partial MICAUPD1 import and the signed product identity

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-15 06:57

## Description

User decision 2026-09-15: the update-package design proposed by mica-build is
accepted. A product release signs one deployment descriptor and ships it as up
to three MICAUPD1 archives with the same signed descriptor: full (`.micaupd`,
every object), root (`.root.micaupd`, the rootfs objects, only when the
kernel id is unchanged) and kernel (`.kernel.micaupd`, the kernel objects,
only when the rootfs id is unchanged). mica-core is the reader and goes first.
No compatibility with the previous descriptor, catalog or archive rules.

Design:

1. **Partial import** (`crates/mica-deploy/src/acquisition.rs`): the archive's
   object count may be anything from 0 to the descriptor's object count. Every
   archive object is still one the descriptor names, listed once, with its size
   and sha256 checked while streaming; an unnamed, duplicate or wrong-sized
   object is refused. After the stream every descriptor object missing from
   the archive must already be present (installed in the store or verified in
   the acquisition workspace), otherwise the import is refused with
   "deployment objects are incomplete". The "archive object count mismatch"
   rule is removed.
2. **Product identity**: the descriptor schema becomes `mica/deployment/v2`
   with a required, signed `product` (an identifier, as `products/<name>` of
   mica-build: `x64-dev`, `x64-minimal`). The device's product is the single
   `PRODUCT=<name>` line of `/usr/lib/mica/product.conf` in the running
   (dm-verity authenticated) root; a missing file, a missing, repeated, quoted
   or malformed PRODUCT line is refused. Acquisition (`check`, `fetch`,
   `import`) and `install` refuse a deployment whose product differs.
3. **Catalog** (`crates/mica-deploy/src/catalog.rs`): schema
   `mica/catalog/v2`; a channel head is `{board, product, channel,
   releaseId, generation}`; heads, the per-channel generation uniqueness and
   the online selection are keyed by board + product + channel.
4. **Contract** (`crates/mica-deploy/tests/component-contracts/`, copied by
   mica-build): `cases.json` gains the device product file (`product`:
   path, the key and the fixture's file content), product cases (matching
   accepted, another product refused, a descriptor without `product` refused)
   and archive cases (full; kernel-only and root-only with the other objects
   present, accepted; partial with an absent object, refused; an unnamed
   object, refused); `deployment.json`, `envelope.json` are regenerated.
   Tests drive both case lists against the reader.

Unchanged: the envelope and signature, board and architecture, monotonic
generation, object digests, the boot-time kernel and support binding, verity
against the kernel trust certificate. No minimum running release.

Acceptance: the contract cases pass in `crates/mica-deploy/tests`; `make check`
and `make pool` with the package gate pass; pushed with CI green; a new
mica-core release in the release lock format is published and reported.

## ActiveForm

Adding partial update archives and the signed product identity to mica-deploy

## Dependencies

- mica-build: writes `product` into the descriptor, `mica/catalog/v2` heads and
  the three archives, and copies the contract files.
