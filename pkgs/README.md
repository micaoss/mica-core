# Package producers

A producer builds and packs a set of this workspace's binaries into Debian
packages. Each producer is a directory here:

| Producer | Binaries | Packages |
| --- | --- | --- |
| `micad` | `micad` (also `mica-apid`, a symlink to it) | `micad`, `mica-apid`, `mica-apid-ui` (the web console, optional) |
| `mqtt` | `mica-mqttd`, `mica-mqtt-broker` | `mica-mqttd`, `mica-mqtt-broker` |
| `sftp` | `mica-sftp-server` | `mica-sftp-server` |
| `deploy` | `mica-deploy` | `mica-deploy` |
| `lifecycle` | `mica-runkit` (static) | `mica-lifecycle` |

`copyright` here is shared by every package.

## A producer

```
pkgs/<producer>/
  producer.env                 PACKAGES, ENABLEMENT, CONTEXTS
  prepare.sh                   compiles and stages the binaries (scripts/build/build-deb.sh)
  Dockerfile                   stages each payload and runs pack.sh
  Dockerfile.dockerignore      only control/*.control enters the build context
  control/<package>.control    one template per package
  maintainer/<package>/        optional preinst/postinst/prerm/postrm
```

`producer.env` holds plain assignments only:

| Key | Meaning |
| --- | --- |
| `PACKAGES` | The packages the producer emits; one control template each |
| `VERSION` | `<upstream>-<revision>`, the version of every package it emits |
| `SOURCE_DATE_EPOCH` | Seconds; every mtime in its packages, bumped with `VERSION` |
| `ENABLEMENT` | `<package>=<n>`: how many `multi-user.target.wants` links each package ships |
| `CONTEXTS` | Extra build contexts, `<name>=<repository path>` (the units under `crates/*/dist`) |

`scripts/deb/producers.sh` discovers and validates them.

## Building

```
make deb MICA_ARCH=<amd64|arm64>
  -> bash scripts/deb/build.sh --producer <p> --arch <arch>     for every producer
       prepare.sh -> scripts/build/build-deb.sh                  compile, stage
       docker buildx build pkgs/<p>/Dockerfile                    pack in the target image
  -> _out/debs/<arch>/pool/<package>_<version>_<arch>.deb
make pool                                                        both architectures, indexed
make package-gate
```

The build contexts every Dockerfile receives are `packer` (`scripts/deb`, for
`pack.sh`), `pkgs` (for `copyright`), `bin` (the staged binaries) and the
producer's `CONTEXTS`; the build argument `MICA_BUILD_BASE` is the
`base` image of `locks/mica-build-env.lock` for the target architecture, and
`BUILDKIT_SYNTAX` is that lock's `docker/dockerfile:1`.

## Rules

- **Producer boundary.** A producer compiles into its own
  `_out/target-deb/<producer>/`, and `build-deb.sh` fails if that directory
  holds a binary the producer does not own.
- **Packing at the target architecture.** `dpkg-shlibdeps` resolves the
  libraries of the container it runs in, so `pack.sh` refuses an architecture
  other than the container's.
- **Version.** `producer.env` declares `VERSION="<upstream>-<revision>"` for
  every package of the producer; the upstream part is the crate version of its
  binaries (`scripts/build/check.sh` asserts it), which also print it
  (`--version`). A packaging-only change bumps the revision; a source change
  bumps the crate version and the upstream part and resets the revision. No
  commit, date or release is in a version or a control field. A dependency on
  `micad` names micad's declared version literally, so a micad bump bumps the
  revision of its dependents.
- **Reproducibility.** `producer.env` declares `SOURCE_DATE_EPOCH`, bumped with
  `VERSION`; `pack.sh` sets every mtime to it and owns everything by root. The
  package gate rebuilds a producer on an empty cache and compares the bytes.
- **Inputs.** `scripts/deb/inputs.sh` hashes the tracked files that determine a
  producer's bytes per architecture into `_out/debs/<arch>/inputs.tsv`; a
  release records it as each pool layer's `mica.inputs`, and
  `scripts/build/reuse.sh` refuses a package whose inputs or bytes changed while
  its version stayed the released one.
- **Enablement is payload.** A unit that starts at boot is enabled by a
  `multi-user.target.wants` symlink inside the package, never by a maintainer
  script; `ENABLEMENT` states the count and the gate checks it.
- **No conffiles.** The root is an immutable dm-verity image.
- **Dependencies.** A dependency on another package built here is pinned to the
  exact version; `micad` depends on `mica-system` (from mica-system-base), which
  owns the `/var/lib/mica` and `/mica` mounts.
- **Maintainer scripts** are POSIX sh; the MQTT packages' `postinst` create their
  service accounts, which is why they depend on `passwd`.
