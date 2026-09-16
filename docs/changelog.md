# Changelog

## 2026-09-16 09:12 [progress]

The build env moves to mica-build-env `20260916-0735`:

- `locks/mica-build-env.lock` is that release's asset unchanged and
  `locks/pins/mica-build-env.pin` records it with the trust hash
  `7df0af68761a63c6517b37a739a57ce947da53fbe558aba2646368e53724bf0a`. The
  images mica-core reads are base and rust; the new `bsp` image is for the
  board repositories and is never read here.
- The packages rebuild byte-identically under the new images: on amd64 all six
  at `0.1.0-1` reproduce the bytes released as `20260915-1135`. On arm64 they do
  not, but the cause is not the images: the same station with the old
  `20260915-0138` lock produces exactly the same arm64 bytes, because
  `docker buildx build --platform linux/arm64` on an amd64 host is QEMU
  emulation while the released archives were built natively. The record
  `docs/task/20260916-0912-emulated-arm64-bytes.md` holds what is still unknown.

## 2026-09-16 08:12 [progress]

`mica-apid` is `0.1.0-2`, and the inputs hash stops counting check-only files:

- The pipefail fix in `crates/mica-apid/ui/verify-ui-policy.sh` changed a file
  inside the apid crate tree, so the inputs guard refused the package at its
  released version. The script builds nothing the package carries, so it, the
  UI's `run.sh`, `eslint.config.js` and `playwright.config.ts` are now excluded
  from `scripts/deb/inputs.sh` alongside the test files -- repository
  housekeeping must not look like a package that changed
  (`mica:docs/decisions/2026-09-15-package-versions.md`, Rationale).
- Narrowing the manifest changes the hash too, so `pkgs/apid/producer.env`
  declares `VERSION="0.1.0-2"` with its epoch unchanged; the archive's bytes are
  the same as `0.1.0-1` apart from the version. Every other producer stays at
  `0.1.0-1`, and `micad (= 0.1.0-1)` is unchanged in its dependents.

## 2026-09-15 10:59 [progress]

Packages are locked by their declared version (task `20260915-1059-package-versions`,
user decision 2026-09-15, R0-R8):

- Every producer declares `VERSION="0.1.0-1"` and `SOURCE_DATE_EPOCH` in its
  `producer.env`; the `VERSION` file and `scripts/deb/version.sh` are gone. No
  version or control field carries a commit, date or release, and
  `Mica-Source-Commit` is removed.
- `micad --version` and `mica-apid --version` print the package version; micad's
  system information reports it as the daemon version and no longer reports a
  commit, a git stamp or a commit date. The diagnostic snapshot schema is 6 and
  its redaction schema 8.
- `scripts/deb/inputs.sh` hashes each producer's inputs per architecture; a
  release records it as `mica.inputs` on each pool layer, and pool manifests
  carry only `mica.source-repo` and `mica.arch`, so an unchanged pool keeps its
  digest.
- `scripts/build/reuse.sh` checks the packages against the newest release under
  the rules, in CI and before a release: a package at its released version must
  keep its inputs and bytes, and a lower version is refused.
- The rootfs component is `mica/rootfs/v2`, without a `version`: like the kernel
  component, its id changes only with its content, so a root rebuilt from the
  same inputs keeps its id. The release version stays in `mica/deployment/v2`.
  The contract fixtures are regenerated. The root carries no
  `/usr/share/mica/release-identity.env` any more, and micad reads none.

## 2026-09-15 07:02 [progress]

Update packages (task `20260915-0657-update-packages`, user decision
2026-09-15):

- A deployment descriptor is `mica/deployment/v2` with a signed `product`; the
  device reads its own from `PRODUCT=` in `/usr/lib/mica/product.conf`, and
  `check`, `fetch`, `import` and `install` refuse another product.
- The catalog is `mica/catalog/v2`: channel heads, generation uniqueness and
  selection are keyed by board, product and channel.
- `import` accepts a MICAUPD1 archive carrying a subset of the descriptor's
  objects (root-only or kernel-only updates) when the rest is already present.
- The contract cases gain the product file, product cases and archive cases;
  the fixtures are regenerated with product `x64-dev`.

## 2026-09-15 01:15 [progress]

Release lock migration, stage 3 (`mica:docs/design/release-lock.md`):

- Inputs: `locks/mica-build-env.lock` (mica-build-env `20260915-0138`, unchanged)
  with `locks/pins/mica-build-env.pin`, and `locks/upstream.lock`, which pins
  nothing of this repository's own. `build-env-image.lock`, `build-env-release`,
  `scripts/build/build-env.sh` and its test are gone.
  `scripts/build/check-lock.sh` applies the lock, upstream and pin rules,
  `scripts/build/locks.sh check|verify` reads `locks/`, and `from.sh` resolves
  `rust`, `base` and the upstream images by digest. The buildkit image of the
  container builders and the Dockerfile frontend come from the lock's
  `upstream` rows.
- Release: the pools `ghcr.io/micaoss/mica-core:pool.<arch>.<release>`, one
  manifest per architecture, and a GitHub Release carrying exactly
  `mica-core.lock` (release, pool and package rows) and `SHA256SUMS`. No `.deb`
  assets and no `core-pkgs.lock`.
- `make locks-test` runs the specification's vectors (`tests/vectors/`) and the
  reader tests; `make release-test` checks the written lock.

## 2026-09-14 20:46 [progress]

`make offline` (`scripts/build/offline.sh`) builds both pools from a clean
checkout with only the pinned inputs and prints where they are:
`_out/debs/<arch>/pool/`, `Packages`, `SHA256SUMS` and `manifest.txt`, the same
outputs `make pool` writes. It refuses a dirty tree and checks the build-env
record offline.

## 2026-09-14 19:40 [progress]

A release also publishes its archives as the OCI artifact
`ghcr.io/micaoss/mica-core:pool.<tag>` (one manifest, one layer per archive of
both architectures) and attaches `core-pkgs.lock`, one row per archive with its
version, sha256 and the pool by digest; `SHA256SUMS` covers the lock. The pushed
tag is never re-pointed and everything is read back with no credential. The
publisher test covers the push, a rerun, a re-pointed tag, a private package
and a refused token against a fake registry. Takes effect from the next release.

## 2026-09-14 11:50 [progress]

The build environment moves to `micaoss/mica-build-env` release
`20260914-1129`: its `build-env-image.lock` (sha256 `2ef37b0cf243…`) replaces
the previous one whole and `build-env-release` records the tag and its
`SHA256SUMS` hash `6c582b2a6ff7…`. `make check`, the amd64 pool and the
package gate with its rebuild pass on the new images.

## 2026-09-14 11:29 [progress]

The signed update contract drops the former project name (user decision): the
readers accept only `mica/deployment/v1`, `mica/kernel/v1`, `mica/rootfs/v1`,
`mica/update-envelope/v1` and `mica/firmware/v1`, and offline archives
(`.micaupd`) only with the magic `MICAUPD1`; nothing else is accepted. The
contract fixtures in `crates/mica-deploy/tests/component-contracts/` are
regenerated by `examples/component-contract-fixtures.rs` with TEST-ONLY keys
derived from labelled seeds, with the x64 kernel release `6.12.107`;
`tests/contract_fixtures.rs` holds the committed bytes to the generator. No
name from before the Mica OS rename remains in the tree.

## 2026-09-14 10:49 [progress]

The remaining names from before the Mica OS rename are mica: the UI package
name in `bun.lock`, the UI's backup file extension `.micabak` and its uptime
caption key, and test fixture names. The one exception is the signed update
contract shared with mica-build (its six schema ids, the offline archive
magic and extension, and the contract fixtures), which is a published
interface and moves together with mica-build.

## 2026-09-14 06:20 [progress]

CI and release builds use caches (`actions/cache` v6): third-party cargo
artifacts, the cargo registry and the bun package cache, keyed by the build-env
lock, the Cargo manifests and lock and `bun.lock`. Only pushes to `main` save,
after `scripts/build/cache-prune.sh` strips the workspace crates' own
artifacts; pull requests and releases only restore. No gate is skipped; a
package built from the pruned cache was checked byte-identical to one built
from scratch (micad, mica-sftp-server).

## 2026-09-14 05:07 [progress]

The task and plan records are cleared: every record described work or proposals
from before the repository was reset (the dropbear/SFTP work, the workspace
convergence, the build-env v0.0.1 move, the independent apid upgrade and the
per-producer rebuild proposals). The unwired acceptance script of the
independent-apid-upgrade proposal, `scripts/gate/interface-dependency-test.sh`,
goes with it.

## 2026-09-14 04:51 [progress]

The image profile is gone from micad: `/usr/lib/mica/profile.conf` is no
longer read (no package ships it since mica-system-base dropped
`mica-profile-dev` and `mica-profile-prod`), and `micad` no longer depends on
`mica-profile`. First-boot provisioning seeds `access.ssh.enabled = false` as
before. Development and production images are to differ through the kernel
command line; micad reads nothing for that yet.

## 2026-09-14 04:13 [progress]

Simplification: comments and old compatibility baggage trimmed.

- The crate that builds `micad` is `crates/micad` (package, library and
  executable `micad`); `mica-core` names only the repository.
- The packaging layer is reduced to what this repository uses.
  `producer.env` has three keys (`PACKAGES`, `ENABLEMENT`, `CONTEXTS`);
  every producer builds amd64 and arm64 with a `prepare.sh`; the package gate
  keeps every check (package set, architecture, one stamp, no overlap or
  `Replaces`, exact local pins, copyright, enablement, no conffiles, POSIX
  maintainer scripts, byte-identical rebuild) without the import-lock,
  virtual-package, `all`-architecture and instance machinery. Removed: the OCI
  registry and lock scripts, the pre-flight, `scripts/deb/README.md`,
  `scripts/build/build-target.sh` and `build-aarch64.sh`, `deps/`.
- Comments across the Rust crates, the UI, the scripts, the units and the
  D-Bus policy are cut to what explains the code: internal plan, milestone and
  section references, pointers to documents outside this repository and
  historical narrative are gone. The published OpenAPI descriptions keep their
  content and lose only those references. Package descriptions are short and
  user-facing.

## 2026-09-14 04:10 [progress]

Architecture and development documentation for readers outside the project:
`docs/architecture.md` (components, the management-plane model, access,
updates and boot, security boundaries, source map), `docs/development.md`
(prerequisites, building, the cargo loop in the build image, running the
daemons locally, the UI, common changes, releasing) and component designs in
`docs/design/` (micad, apid, MQTT, deployments and boot, packaging and
release). `pkgs/README.md` names the current make targets and paths.

## 2026-09-14 03:30 [progress]

mica-core moves to the `micaoss` organisation with a fresh history, following
mica-podman. `ci.yml` runs the gates and builds, packs and gates every package
for amd64 and arm64 on push and pull request (a docs- or markdown-only change
builds nothing) and publishes nothing; `release.yml` runs only when a release
`<YYYYMMDD-HHMM>` is published, builds that tag and attaches every archive and
`SHA256SUMS` to it (`scripts/build/release.sh`, read back anonymously, tested
by `make release-test`). The shared build (`build.yml`) compiles and packs
arm64 on arm64 runners with no emulation (`scripts/build/build-deb.sh`
compiles in the host's image); `scripts/deb/package-gate.sh` gates one
architecture (`--arch`) or both pools without rebuilding (`--no-reproduce`).
The OCI pool publication, the `v<VERSION>` tag release and the published-pool
build decision are gone. No file names the former organisation. Not yet
published.

## 2026-09-14 02:40 [progress]

`build-env-image.lock` is the lock of the mica-build-env release
`20260914-0128`, now published from `micaoss/mica-build-env`: the loader
downloads from `github.com/micaoss/mica-build-env` and accepts only
`ghcr.io/micaoss/mica-build-env` references, with no fallback to the old
organisation's namespace. Every image digest is new (Python moved from the C image
to the base image), so the full gates have not yet run in these images. Not
yet published.

## 2026-09-14 02:00 [progress]

The build-env images come from `build-env-image.lock` at the repository
root: the lock asset of the mica-build-env release `20260914-0042`,
committed unchanged, with `build-env-release` recording that tag and the
sha256 of its `SHA256SUMS`. `make deps` verifies the lock against that
release anonymously and `make deps-check` checks both files offline
(`scripts/build/build-env.sh`, tested by `make build-env-test`). The old
`deps/build-env.json` pin and the fetched `build-env/` directory are gone,
because the releases they named and their images no longer exist. All four
image digests are new, so every package producer builds in new image bytes
and the full gates have not yet run in them. The pool decision compares the
Rust and base images of the verified locks of both commits. Not yet
published.

## 2026-09-14 01:10 [progress]

CI no longer runs on a branch push or pull request. A release is manual, as
in bkhq/bkd: bump `VERSION` by a commit on main and push the tag
`v<VERSION>`, or dispatch `release.yml` with it. The workflow checks the tag
(`scripts/build/release.sh check`: `v<VERSION>` on main), runs every gate,
publishes the pool and then the GitHub release of the tag, whose notes name
both pools by manifest digest and every archive by sha256, read back with no
credential. Dispatched without a tag it is a dry run that publishes nothing.
A release whose commit changes no package input since the last published
pool is refused. Not yet published.

## 2026-09-14 00:55 [progress]

The package producers moved from `packaging/deb/<producer>/` to
`pkgs/<producer>/` (history kept), with the shared copyright file and the
packaging contract at `pkgs/copyright` and `pkgs/README.md`; each producer's
`family` build context is `pkgs`. Producer compile outputs moved from the
root `target-deb/` to `_out/target-deb/`. Package contents are unchanged.
Not yet published.

## 2026-09-14 00:40 [progress]

The mica-build-env pin is the new v0.0.1 (`SHA256SUMS` de740ff5798e), cut
after mica-build-env's history and release reset; the earlier v0.0.1 and
v0.0.2 and their image digests no longer exist. Its generated `images.env`
holds only the four `IMAGE_MICA_BUILD_*` references, which is all this
repository reads. The Rust and base images are new digests with the same
toolchain versions. Not yet published.

## 2026-09-13 23:55 [progress]

CI builds and publishes a pool only for a commit that needs one
(`scripts/build/pool-decision.sh`). When everything changed since the nearest
ancestor with a complete published pool (both architectures, this
repository, that commit) is docs, markdown, `.gitignore` or a release pin
whose verified releases pin `IMAGE_MICA_BUILD_RUST` and
`IMAGE_MICA_BUILD_BASE` to the same repository and digest (a tag may differ), the commit builds nothing and gets no pool; any other
change, and anything the step cannot establish, builds the full pool with
every existing gate. Not yet published.

## 2026-09-13 23:10 [progress]

The mica-build-env pin is v0.0.2 (`SHA256SUMS` b75932fc6df3). Its `RULES.md`
and all four `IMAGE_MICA_BUILD_*` digests equal v0.0.1's; it drops the
rust-check image this repository no longer used. Not yet published.

## 2026-09-13 22:40 [progress]

Built on the mica-build-env v0.0.1 release (`20260913-2200-build-env-release-v0.0.1`).
`deps/build-env.json` pins the version and the sha256 of its `SHA256SUMS`;
`make deps` (`scripts/build/build-env.sh`) downloads the release assets and
refuses them unless both hashes match. The Rust gate, the builds, the
boot/shutdown and IO fault suites run in the published `IMAGE_MICA_BUILD_RUST`,
the UI build and the packing in `IMAGE_MICA_BUILD_BASE`, both by digest; no
`LOCAL_MICA_BUILD_*` image is built or named. The scripts that run are this
repository's own copies of the release reference implementation
(`scripts/build/from.sh`, `scripts/deb/`); `tools/deps.sh` and the source pin
are gone, and CI no longer builds images. The independent mica-apid upgrade
is a proposal only (`docs/plan/20260913-2230-independent-apid-upgrade.md`).
Not yet published.

## 2026-09-13 21:20 [progress]

mica-apid is its own executable and producer; micad no longer carries apid.
Every name in this repository that predates the Mica OS rename is mica, with no compatibility: the
D-Bus object /com/mica/micad, MICAD_* variables, the mica account, the pool
subdirectory, boot entry, verity and U-Boot names, and the core-owned schema
ids. The signed update contract shared with mica-build (the deployment,
kernel, rootfs, update-catalog, update-envelope and firmware schema ids and the
archive magic) waits for mica-build's re-signed fixtures. Not yet
published.

## 2026-09-13 20:45 [progress]

The apid executable is `mica-apid` (`/usr/bin/mica-apid`, a link to `micad`;
`micad` keeps its name) and the system DATA namespace is mounted at `/mica`,
both on the user's request. Units, packages, D-Bus names
and API keys are unchanged. Not yet published.

## 2026-09-13 20:16 [progress]

One crate workspace (`20260913-1935-workspace-convergence`, phases 1 and 2):
every crate under `crates/`, the producers under `packaging/deb/`, the shell
entry points under `scripts/build/` and `scripts/gate/`. The Cargo packages
`micad` and `apid` are now `mica-core` and `mica-apid`; executables, Debian
packages, units, D-Bus names and installed paths are unchanged. mica-deploy
joined with its history (b698b10dd6aa, merged unchanged, then moved into
`crates/mica-deploy`, `crates/lifecycle-sys`, `packaging/deb/{deploy,lifecycle}`
and `scripts/gate/`), so the pool has seven packages; `mica-runkit` is built
alone on the static route. Not yet published.

## 2026-09-13 03:30 [progress]

Created from the daemon package directory of `micaoss/mica-build` (163 commits kept through
`git subtree split`, then the tree at the Mica OS rename). Moved in with the
workspace: the Rust gate driver (`gate/rust-gate.sh`), the built-in UI's
build contract test (`gate/apid-ui-build-contract-test.sh`) and the D-Bus
policy test (`tests/dbus-policy-test.sh`). The API harness that boots the
assembled image stays in the assembly (`mica-build:tests/apid-api/`); for
its build-time half the `mica-apid` archive now ships
`/usr/share/mica-apid/openapi.json`. The substrate is the `mica-build-env`
source pin at `build-env/`; the four packages are published as
`build-<commit12>` and imported by `mica-build` through `deps/packages/`
(Phase 5 of `20260911-2006-split-package-repositories`).
