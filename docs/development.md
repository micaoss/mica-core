# Developing mica-core

This guide covers building, testing and changing mica-core. Read
[`architecture.md`](architecture.md) first for what the parts are.

## 1. Prerequisites

The host needs no Rust or Node toolchain: compilation, the UI build, packing and
the gates run in the images `locks/mica-build-env.lock` pins.

| Needed | For |
| --- | --- |
| Linux, x86-64 or arm64 | Everything (some suites run only on x86-64, see below) |
| docker with buildx | Every build and gate |
| bash, git, make, curl, jq, sha256sum | The scripts |
| root, `dbus-daemon` and python3 on the host | `make dbus-policy-test` only |

## 2. First build

```sh
git clone https://github.com/micaoss/mica-core.git
cd mica-core
make deps          # check locks/ and verify every pinned lock against its release
make check         # lint, loader and publisher tests, UI contract, Rust gate, boot/shutdown, IO faults
make deb MICA_ARCH=amd64
make offline      # both pools from a clean checkout, pinned inputs only; prints the output paths
```

`make help` lists every target. All outputs go under `_out/` (gitignored):
`_out/cargo` is the shared crate download cache, `_out/rust-gate` the gate's
target directory, `_out/target-deb/<producer>` the producers' target
directories, `_out/apid-ui/dist` the built UI, `_out/debs/<arch>/pool` the
archives.

`make check` takes a while on a cold cache; the individual targets
(`make rust-gate`, `make boot-shutdown-test`, ...) run one suite each.
`boot-shutdown-test` and `file-transaction-faults` need an x86-64 host.

### A local archive is not the published archive

`scripts/build/build-deb.sh` runs the rust image on the **host** architecture
and cross-compiles to the target (`--target aarch64-unknown-linux-gnu`, linked
with `aarch64-linux-gnu-gcc`). On an x86-64 workstation the arm64 archives are
therefore cross-built, while CI builds them natively on an arm64 runner. The
two do not produce the same bytes: measured on 2026-09-16, the six packages at
`0.1.0-1` rebuilt byte-identically on amd64 and all six differed on arm64,
because a cross build carries the cross linker's `.note.package` and a
different cargo crate disambiguator. The compilers are the same
(`.comment` is identical), and the same station with the previous build-env
lock produces the same bytes, so this is not a toolchain change.

What follows for anyone comparing a local build to a release:

- The published archives are the native ones. A locally built arm64 archive is
  a valid archive and is not the one a release carries.
- `scripts/build/reuse.sh` run locally cannot validate the arm64 half on an
  x86-64 station; every arm64 package will look changed. CI is the authority:
  its `gate` job runs the same guard over natively built artifacts.
- `make offline` on an x86-64 station produces arm64 archives that differ from
  the released ones. That is what offline means here.
- amd64 is unaffected: a local amd64 archive does reproduce the released bytes.

This bound is **provisional**. Building the pool on the target platform
everywhere would close it, and it is deferred only because it would cost a
version bump on every package and change nothing that ships;
`docs/task/20260916-0912-cross-built-arm64-bytes.md` holds the measurements and
the decision.

## 3. Working with cargo

The Rust gate is `scripts/gate/rust-gate.sh`: it builds the UI, then runs
`scripts/build/check.sh` in the Rust image with the repository mounted
read-only at `/src`. For an interactive loop, open a shell in the same image:

```sh
bash crates/mica-apid/ui/build.sh          # once, and after UI changes; apid embeds the result
IMAGE=$(bash scripts/build/from.sh --ref rust)
mkdir -p _out/cargo/registry _out/cargo/git _out/dev-target
docker run --rm -it \
    -v "$PWD:/src" -w /src \
    -v "$PWD/_out/cargo/registry:/usr/local/cargo/registry" \
    -v "$PWD/_out/cargo/git:/usr/local/cargo/git" \
    -v "$PWD/_out/dev-target:/target" -e CARGO_TARGET_DIR=/target \
    -v "$PWD/_out/apid-ui/dist:/build/apid-ui:ro" -e MICA_APID_UI_DIST_DIR=/build/apid-ui \
    --entrypoint /bin/bash "$IMAGE"
```

Inside it:

```sh
cargo nextest run -p micad            # one crate's tests
cargo nextest run --workspace --locked    # everything
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all
```

`mica-apid` refuses to build without `MICA_APID_UI_DIST_DIR` pointing at a built
UI, because the binary embeds it.

### 3.1 Running the daemons locally

Never point a daemon at the host's system bus or files. The bus tests show the
safe shape (`crates/micad/tests/bus.rs`): start a private
`dbus-daemon --session --print-address=1 --nofork`, create a settings file and a
config directory in a temporary directory, and run micad with

```sh
DBUS_SESSION_BUS_ADDRESS=<address> MICAD_BUS=session MICAD_DRY_RUN=1 \
MICAD_SETTINGS_PATH=<tmp>/settings.toml MICAD_CONFIG_DIR=<tmp>/config \
MICAD_SHADOW_PATH=<tmp>/shadow  micad
```

`MICAD_DRY_RUN=1` constructs no reconcilers and touches no host state. apid
joins the same bus with `APID_BUS=session`, and listeners and state of its own:
`APID_HTTPS_ADDR`, `APID_HTTP_ADDR`, `APID_STATE_DIR`.

## 4. The web UI

`crates/mica-apid/ui/` is React + Vite with TanStack Router, Tailwind and
i18next, built by Bun.

- The shipped build always runs in the base image
  (`bash crates/mica-apid/ui/build.sh`); a host Bun produces different chunk
  hashes. `crates/mica-apid/ui/run.sh` runs the same build with its checks.
- For UI work, `bun install` then `bun run dev:bare` serves it under `/_ui/`;
  `bun run lint`, `bun run typecheck`, `bun run test` (Vitest) and
  `bun run test:e2e` (Playwright) are the UI's own checks.

## 5. Making changes

### 5.1 Conventions

- Rust edition 2024, `rust-version` 1.96, workspace lints: `unsafe_code` is
  forbidden everywhere except `lifecycle-sys`; clippy warnings are errors.
- `Cargo.lock` is committed and every build is `--locked`; dependency licences
  and advisories are checked by `cargo deny` (`deny.toml`).
- Every crate's `[package] version` equals `VERSION`.
- Shell is bash with `set -euo pipefail`; never put `grep -q` on the right of a
  pipe (use `grep -c ... >/dev/null`).
- Documentation lives in `docs/`; work is tracked in `docs/task/`, larger
  designs in `docs/plan/`, and user-visible changes in `docs/changelog.md`.

### 5.2 Adding a setting

1. Add the field to the model in `crates/micad-settings/src/model.rs`, with
   validation, and decide which document carries it
   (`crates/micad-settings/src/documents.rs`); a new field in a persisted
   document is a schema change of that document.
2. Make the reconciler that owns the subtree apply it
   (`crates/micad/src/reconciler/`), or add a reconciler and list it in
   `reconciler::all()`.
3. If apid exposes it beyond `GET/PUT /api/v1/settings/{path}`, add the route
   in `crates/mica-apid/src/` and regenerate the OpenAPI document (5.4).

### 5.3 Adding a D-Bus member

Add it to the `com.mica.micad1` implementation in
`crates/micad/src/bus.rs`, extend the bus tests in
`crates/micad/tests/bus.rs`, and add it to apid's proxy in
`crates/mica-apid/src/bus_client.rs` if apid calls it. Additive changes keep the
interface name; a breaking change needs a new interface.

### 5.4 Changing the HTTP API

Handlers and their OpenAPI annotations live in `crates/mica-apid/src/`. After a
change, regenerate the committed document:

```sh
MICA_APID_UI_DIST_DIR="$PWD/_out/apid-ui/dist" \
  cargo run -p mica-apid --bin mica-apid -- --openapi > crates/mica-apid/openapi.json
```

(inside the Rust image shell). The Rust gate fails when the committed document
and the binary disagree.

### 5.5 Adding a binary or a package

1. Add the crate under `crates/<name>/`.
2. Add a producer `pkgs/<producer>/` with `producer.env`, `Dockerfile`,
   `Dockerfile.dockerignore`, `control/<package>.control` and a `prepare.sh`
   that calls `scripts/build/build-deb.sh --bins <binaries>`; add the binary to
   `ALL_BINARIES` in `scripts/build/build-deb.sh`.
3. Run `make deb MICA_ARCH=amd64` and `bash scripts/deb/package-gate.sh --arch amd64`.

`pkgs/README.md` describes producers and the packaging rules.

### 5.6 Moving to another input release

Download the release's `<repository>.lock` and `SHA256SUMS`, check that
`SHA256SUMS` hashes to the value the release announces and lists only the lock,
then replace `locks/<repository>.lock` with the downloaded file and
`locks/pins/<repository>.pin` with a pin naming the release and that hash,
together and nothing else. `make deps` must pass. A third-party image is used
only if `locks/mica-build-env.lock` lists it (`scripts/build/from.sh --upstream`).

## 6. CI and releasing

- Every push to `main` and every pull request runs `ci.yml`: the gates and a
  native amd64 and arm64 build, pack and gate. Keep it green.
- A release is cut on a commit of `main` with
  `gh release create <YYYYMMDD-HHMM> --target <commit>`; `release.yml` builds
  that tag, pushes the pools and attaches `mica-core.lock`. See
  [`design/packaging-and-release.md`](design/packaging-and-release.md).
