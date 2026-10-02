# 20261002-1200-catalog-references-the-descriptor The catalog references each release's descriptor as a file

- **status**: done
- **priority**: P1
- **owner**: mica-core
- **createdAt**: 2026-10-02 12:00

## Description

Raised by mica-res (user decision, 2026-10-02). Today `mica/catalog/v1`
carries each release's signed descriptor **inline**: the whole
`mica/update-envelope/v1` (3-4 KB of base64, escaped as a JSON string inside
the catalog) under `releases[].deployment`. That was inherited from a catalog
that was signed and listed every release, so one request brought every
candidate. Neither holds any more: the catalog is unsigned and offers **the
current release of each product only**, so a device needs exactly one
descriptor -- its own product's.

The resource service now keeps everything of a release in one directory, the
descriptor included, so the catalog points at it instead of embedding it:

```
https://dl.res.micaos.dev/mica/<product>/<stamp>/
  mica-<product>-<stamp>.img.gz        image
  mica-<product>-<stamp>.micaupd       full update archive
  index.json                           the release's document
  deployment.json                      the signed descriptor (the envelope, byte for byte)
  objects/<sha256>                     each object the descriptor names
```

**Trust does not change.** A device still trusts only the release key's
signature on the descriptor, the digests and lengths inside it, the product it
names and the generation it runs above. The catalog's own fields are hints
that select which descriptor to fetch; the descriptor then has to agree with
them.

A device also should not download every product's release detail to learn
that it is current. res therefore serves two levels. Every document that
links to others carries a `baseUrl` (ending in `/`) and names each link as a
`path` relative to it; the URL is `baseUrl + path`. Links stay short, and an
operator can point a device at a mirror or another CDN by replacing `baseUrl`.

| Address | Schema | `baseUrl` | What it holds |
|---|---|---|---|
| `https://res.micaos.dev/update/v2/manifest.json` | `mica/catalog/v2` | `https://dl.res.micaos.dev/` | one line per product: `product`, `board`, `variant`, `latest {id, generation, notes, path}`, and the path of the product's `releases.json`. No objects. `no-cache` |
| `<manifest baseUrl><latest.path>` = `https://dl.res.micaos.dev/mica/<product>/<stamp>/index.json` | `mica/release/v1` | the release's directory | that release, complete: `id`, `product`, `board`, `variant`, `version`, `generation`, `notes`, `publishedAt`, `files[] {..., path}`, `deployment {path, sha256, bytes}`, `objects[] {sha256, bytes, path}`. Written once, never changed |
| `<manifest baseUrl><releases>` = `https://dl.res.micaos.dev/catalog/<product>/releases.json` | `mica/releases/v1` | `https://dl.res.micaos.dev/mica/` | history for people; a device never reads it |

A current device downloads the manifest and nothing else; only when its
product's `latest.generation` is above the installed one does it fetch the
release's document, then the descriptor, then the objects.

## Acceptance

- A device configured with the update root `https://res.micaos.dev/update/`
  reads `v2/manifest.json` below it and finds the line for its board and product; when `latest.generation` is not
  above the installed generation it stops there. Otherwise it fetches the
  release's document (manifest `baseUrl` + `latest.path`), requires its `id`, `board`, `product` and
  `generation` to equal the manifest line, fetches the descriptor (the
  document's `baseUrl` + `deployment.path`), accepts it
  only when its sha256 and length match, authenticates it, refuses it when the
  signed board, product or generation differs, and then fetches the objects as
  before.
- The reader refuses: a descriptor whose bytes do not match `deployment.sha256`
  / `deployment.bytes`; two manifest lines for one board and product; a
  `baseUrl` that is not https, has no host, carries user info, a query or a
  fragment, or does not end in `/`; a `path` that is empty, starts with `/`,
  or has a `.`/`..` segment, a `?`, a `#` or a `\`.
- `bash scripts/gate/rust-gate.sh` green; the shared vectors changed in
  lockstep with mica-build.

## The change

`mica/catalog/v1` becomes **`mica/catalog/v2`** (the manifest) plus
**`mica/release/v1`** (each release's own document). Both canonical JSON,
unsigned, as before.

```json
{ "schema": "mica/catalog/v2", "revision": 3, "baseUrl": "https://dl.res.micaos.dev/",
  "products": [
    { "product": "mini-x64.basic", "board": "mini-x64", "variant": "basic",
      "latest": { "id": "mini-x64.basic.20261001-2113", "generation": 2, "notes": "",
                  "path": "mica/mini-x64.basic/20261001-2113/index.json" },
      "releases": "catalog/mini-x64.basic/releases.json" } ] }
```

```json
{ "schema": "mica/release/v1",
  "baseUrl": "https://dl.res.micaos.dev/mica/mini-x64.basic/20261001-2113/",
  "id": "mini-x64.basic.20261001-2113",
  "product": "mini-x64.basic", "board": "mini-x64", "variant": "basic",
  "version": "basic-20261001-2113", "generation": 2, "notes": "",
  "publishedAt": "2026-10-01T13:20:00.000Z",
  "files": [{ "kind": "update", "form": "full", "sha256": "...", "size": 123,
              "path": "<file name>" }],
  "deployment": { "path": "deployment.json", "sha256": "<sha256 of deployment.json>", "bytes": 3363 },
  "objects": [{ "sha256": "...", "bytes": 33838080, "path": "objects/<sha256>" }] }
```

Unknown fields (`files`, `publishedAt`, `variant`, `releases`) are for people
and the website; the reader may ignore them but must still check canonical form.
`heads` goes: with one line per product, a line **is** its head.

`crates/mica-deploy/src/catalog.rs`

| Today | Change |
|---|---|
| `Catalog { schema, revision, heads, releases }` | `Manifest { schema, revision, products: [ProductLine] }`, `Manifest` gains `base_url`; `ProductLine { product, board, variant, latest: { id, generation, notes, path }, releases }` |
| `Release { id, notes, deployment: String, objects }` | `Release` parsed from the release's document: `{ id, product, board, generation, notes, deployment: DeploymentRef, objects }`, `DeploymentRef { path, sha256, bytes }`, plus the document's `base_url`; objects carry `path` |
| line 183, `authenticate_deployment(release.deployment.as_bytes(), keys)` for **every** release | select from the manifest first (no I/O beyond it); only the selected release's document and descriptor are fetched |
| the heads consistency check | one line per `(board, product)`; the release's document must agree with its line |

`verify_catalog` splits into three steps with downloads between them, done by
`acquisition.rs` `check()` under the same size bound as an object: parse and
select from the manifest; parse the release's document and check it against the line;
check the descriptor's digest and length, authenticate it, and require the
signed `board`, `product` and `generation` to equal the entry's and
`objects` to be exactly what the descriptor names (as today).

`docs/mica-core.md`, the catalog paragraph, follows.

## Notes

- **Shared vector**: `tests/component-contracts/catalog.json` and `cases.json`
  change together with `mica-build:tests/fixtures/component-contracts/`; the
  vector now carries the manifest, the release's document and the descriptor as
  three byte strings. mica-res copies it into
  `apps/api/src/modules/resource/__fixtures__/device-catalog-vector.json` and
  checks its writer against it byte for byte.
- **mica-res side** (done there): writes the manifest and `releases.json`, and
  `index.json`, `deployment.json` and `objects/` in each release's directory. Until this task lands a device refuses v2 -- no device reads res
  yet.

## Compatibility (added 2026-10-02, user decision)

An exact schema check at one configured URL strands every device the day the
server changes the document: it refuses the catalog and can never reach a
reader that understands the new one. So:

- The configured source (`updates.json` `source.url`, the baked default) is an
  **update root** ending in `/`; the reader appends the major it reads,
  `v2/manifest.json`. A source that does not end in `/` is refused.
- Within a major a document only grows: `mica/catalog/v2` and
  `mica/release/v1` ignore unknown fields at every level (they are still
  canonical, schema-exact and bounded, and must agree with the signed
  descriptor).
- A change an old reader cannot ignore is a new major at a new path, written
  beside the old one for as long as devices read it; the old major's latest is
  a stepping stone whose reader takes both. The signed descriptor still
  refuses unknown fields, so its majors follow the same stepping-stone rule.

## Outcome

- `catalog.rs`: `verify_catalog(keys, request, fetch)` reads the manifest, and
  only for a newer generation the release's document and the descriptor,
  through `fetch`; `acquisition.rs` `check()` downloads each through the
  staging file under `MAX_CATALOG_BYTES`. `SelectedRelease` is unchanged, so
  `fetch`, the CLI output and micad are untouched. A `path` is also refused
  when the joined URL leaves its `baseUrl` (a `scheme:` first segment), and a
  `baseUrl` must be exactly its parsed form. `baseUrl` may be http only when
  the source is http (local tests), as objects were before.
- Delta transfer is unchanged and now relative to the release's directory: an
  object's index is `objects/<sha256>.index`, its chunks `chunks/<sha256>`
  beside `objects/`; an origin without them serves whole objects.
- The shared vector carries `manifest`, `release` and `descriptor` as exact
  bytes with their URLs (`manifestUrl`, `releaseUrl`, `descriptorUrl`);
  `cases.json` accepts `mica/catalog/v2` and `mica/release/v1`.
- Every producer is 0.0.2.
- `products[].releases` is gone from the manifest (mica-res, user-approved
  2026-10-02): the history is generated by res and served at
  `https://res.micaos.dev/update/v2/<product>/releases.json`, not under the
  manifest's `baseUrl`. No device reads it; the reader never required it, and
  the vector no longer carries it.
