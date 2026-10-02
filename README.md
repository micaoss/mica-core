# mica-core

The management plane of Mica OS: the daemon that owns a device's configuration and turns it
into running services, the API and web console operators use, the MQTT bridge for
application data, and the signed-deployment client and early-boot executable that update and
boot a device around its signed, read-only root.

It runs on the floor of [mica-system-base](https://github.com/micaoss/mica-system-base) under
either init, systemd or OpenRC. [mica-build](https://github.com/micaoss/mica-build) installs its
packages into every product's root and signs its core components into every deployment.
**[docs/mica-core.md](docs/mica-core.md)** describes the components and packages, the daemons, their contracts with the rest of the system, and how this repository is
built, checked and released.

Building and publishing run on [mica-build-tools](https://github.com/micaoss/mica-build-tools)
(`locks/mica-build-tools.pin`) inside the images of
[mica-build-env](https://github.com/micaoss/mica-build-env) (`locks/mica-build-env.lock`).

## Core components and packages

micad and the console are **core components** of a product's deployment, not Debian packages:
each is a squashfs of its `/usr` and `/etc` with a dm-verity hash tree and an unsigned
`mica/core/v1` record, composed over the root's `/usr` and `/etc` at boot, so a mica-core
release reaches devices without a new root.

| Component | Arch | What it is |
| --- | --- | --- |
| `micad` | amd64, arm64 | The management daemon (settings, reconcilers, the root-only `com.mica.micad` D-Bus interface) and the API, the same executable under the name `mica-apid`: plain HTTP on 8080 by default, HTTPS on 8443 when enabled |
| `mica-apid-ui` | amd64, arm64 | The built-in web console, at micad's exact version; without it a device is API-only |

The rest are Debian packages built into the root:

| Package | Arch | What it is |
| --- | --- | --- |
| `mica-mqtt-broker` | amd64, arm64 | A local MQTT 3.1.1 broker, started by micad |
| `mica-mqttd` | amd64, arm64 | The bridge from enrolled `com.mica.*` applications to MQTT, started by micad |
| `mica-sftp-server` | amd64, arm64 | SFTP version 3 for dropbear's `sftp` subsystem |
| `mica-deploy` | amd64, arm64 | Signed deployments: check, fetch (with delta transfer), import, install, confirm, rollback |
| `mica-lifecycle` | amd64, arm64 | `mica-runkit`, the static PID 1 of the signed initramfs and the exit ramdisk |

Each component and package carries the start-up files of its own services, for systemd and
OpenRC. micad and apid enable themselves on both; the MQTT daemons are started by micad. The product states its
init and features in `/usr/lib/mica/product.conf` (`INIT`, `FEATURES`), and micad picks every
backend from them.

## Releases

A release is named by its UTC minute, `YYYYMMDD-HHMM`. It is cut with
`gh release create <YYYYMMDD-HHMM> --target <commit of main>`; publishing it runs `release.yml`,
which builds the tag on both architectures, gates it, checks every package and component against
the previous release and publishes it. `ci.yml` runs the same build and gates on every push and publishes
nothing.

In `ghcr.io/micaoss/mica-core`:

| Tag | Content |
| --- | --- |
| `pool.<arch>.<release>` | The packages and core components for amd64 or arm64: one layer per `.deb`, two per component (its image `<package>_<version>_<arch>.core.img` and its record `.core.json`, pool items of the types `core.img` and `core.json`), each annotated with its producer's `mica.inputs`; an unchanged pool keeps its digest |

On the GitHub release, never replaced (format: `mica-build-tools:docs/spec/release-lock.md`):

| Asset | Content |
| --- | --- |
| `mica-core.lock` | `mica-lock v1`: the `release` row, a `pool` row per architecture, a `package` row per package and architecture, and two `item` rows per component and architecture: `item core.img` (the image's sha256) and `item core.json` (the record's) |
| `SHA256SUMS` | The sha256 of `mica-core.lock` |

A component's version is its binaries' crate version, `<major>.<minor>.<patch>`; a package's adds a
Debian revision, `<major>.<minor>.<patch>-<revision>`. Either is bumped with any change to what
builds it; one at a released version never changes.

## Consuming a release

1. **Pin it.** Commit the release's `mica-core.lock` unchanged as `locks/mica-core.lock` with its
   pin `locks/pins/mica-core.pin` (the release and the sha256 of its `SHA256SUMS`), checked with
   `mica-tools locks verify`. Move both together.
2. **Take the packages** from the `package` rows: the layer of that architecture's `pool` whose
   digest is the row's sha256. Install them into the root.
3. **Take the core components** from the `item` rows of the types `core.json` and `core.img`
   (`mica-tools lock rows locks/mica-core.lock item`): the record layer by its row's sha256, and
   the image layer by its row's, which is the sha256 the record names. Sign each record's root hash with the
   product's key, complete the record with the signature and its id, and add the components the
   product's features select to its `mica/deployment/v1`, over a `mica/rootfs/v1` root whose
   `interfaceLevel` is inside each component's `root` range
   ([docs/mica-core.md](docs/mica-core.md) section 6.5).
4. **Pair it with a mica-system-base release** whose floor and init packages it runs on; the
   services micad drives on OpenRC, and which package ships each, are
   [docs/mica-core.md](docs/mica-core.md) section 3.8.
5. **Write the product file.** `PRODUCT`, `FEATURES` and `INIT` in `/usr/lib/mica/product.conf`,
   and the board section of the signed boot policy, decide what a device serves and how it boots.

## Building

The host needs docker with buildx, bash, git, make, curl and jq, and no Rust or Node toolchain.

```sh
make deps                  # every lock and pin, verified against its release
make check                 # lint, UI contract, Rust gate, boot/shutdown, IO faults
make deb MICA_ARCH=amd64   # the archives and core components, in _out/debs/<arch>/pool
make package-gate          # the package and component rules, including a byte-identical rebuild
```

`make help` lists every target; outputs go under `_out/`.

## Repository layout

| Path | Content |
| --- | --- |
| `crates/micad`, `crates/micad-settings` | The daemon, and the settings tree and the rules it shares with apid |
| `crates/mica-apid` | The API, its committed `openapi.json`, and the console in `ui/` |
| `crates/mica-mqttd`, `crates/mica-mqtt-broker` | The MQTT bridge and broker |
| `crates/mica-sftp-server` | The SFTP server |
| `crates/mica-deploy`, `crates/lifecycle-sys` | Deployments and `mica-runkit`, and the typed Linux device operations (the one crate allowed `unsafe`) |
| `crates/mica-fs`, `crates/mica-busname`, `crates/mica-ui-bundle` | Durable file writes, the `com.mica.*` name rule, console bundles |
| `crates/*/dist` | Each service's systemd unit and OpenRC script, and micad's D-Bus policy |
| `pkgs/` | One producer per directory: its packages or core components, inputs, control or component templates and packing |
| `scripts/` | The producer build, the gates, the package gate and the release publisher |
| `locks/`, `bin/mica-tools` | The pinned build environment and build tools |
| `docs/mica-core.md` | The description of this repository |

## License

Apache License 2.0; see `LICENSE`.
