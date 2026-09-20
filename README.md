# mica-core

The management plane of Mica OS: the `micad` daemon and everything that ships
beside it — the API daemon with its built-in web UI, the MQTT bridge and
broker, the SFTP server, the deployment client and the early-boot lifecycle
executable. It is one Cargo workspace, packed into Debian packages by this
repository's own scripts and released as GitHub Release assets.

Documentation: [architecture](docs/architecture.md),
[development](docs/development.md), and the component designs under
[`docs/design/`](docs/design/) ([micad](docs/design/micad.md),
[apid](docs/design/apid.md), [MQTT](docs/design/mqtt.md),
[deployments and boot](docs/design/deployment.md),
[packaging and release](docs/design/packaging-and-release.md)).

## Deliverables

Seven Debian packages, each for `amd64` and `arm64`. Each producer declares its
packages' version, `<upstream>-<revision>`, and `SOURCE_DATE_EPOCH` in
`pkgs/<producer>/producer.env`; the upstream part is the crate version of the
binaries it ships. A release never changes a version: a package is rebuilt only
when its version is bumped, and an unchanged one keeps its bytes from release to
release. Each archive records its repository in `Mica-Source-Repo` and carries
no commit.

| Package | Built from | Installs | Starts at boot | Depends on |
| --- | --- | --- | --- | --- |
| `micad` | `crates/micad` | `/usr/bin/micad`, `micad.service`, the D-Bus policy `com.mica.micad.conf` | yes | `dbus-system-bus`, `mica-system` |
| `mica-apid` | `crates/mica-apid` | `/usr/bin/mica-apid` (UI embedded), `apid.service`, `/usr/share/mica-apid/openapi.json` | yes | `micad` (same version), `mica-system` |
| `mica-mqttd` | `crates/mica-mqttd` | `/usr/bin/mica-mqttd`, `mica-mqttd.service`; its `postinst` creates the service account | no | `micad` (same version), `passwd` |
| `mica-mqtt-broker` | `crates/mica-mqtt-broker` | `/usr/bin/mica-mqtt-broker`, `mica-mqtt-broker.service`; its `postinst` creates the service account | no | `micad` (same version), `passwd` |
| `mica-sftp-server` | `crates/mica-sftp-server` | `/usr/lib/sftp-server`, the SFTP subsystem dropbear runs | — | shared libraries only |
| `mica-deploy` | `crates/mica-deploy` | `/usr/bin/mica-deploy` | — | `mount` |
| `mica-lifecycle` | `crates/mica-deploy` | `/usr/lib/mica/lifecycle/mica-runkit`, static, reached as `init` and `shutdown` | — | nothing |

Every package also ships `/usr/share/doc/<package>/copyright`. Units that start
at boot are enabled by a `multi-user.target.wants` symlink inside the package;
no maintainer script enables anything. `mica-lifecycle` is read out of the
archive by the assembly's kernel component and is never installed into a root.

A release `<YYYYMMDD-HHMM>` is described in the Mica OS release lock format
(`mica:docs/design/release-lock.md`):

- the archives live only in the OCI pools
  `ghcr.io/micaoss/mica-core:pool.<arch>.<YYYYMMDD-HHMM>`, one manifest per
  architecture with one layer per archive (media type
  `application/vnd.mica.deb`, the archive's file name as
  `org.opencontainers.image.title`);
- the GitHub Release carries exactly `mica-core.lock` (the release row, a
  `pool` row per architecture by digest, a `package` row per archive with its
  version and sha256) and `SHA256SUMS`, which lists only the lock.

A package's sha256 is the digest of its layer. A consumer commits
`mica-core.lock` unchanged as `locks/mica-core.lock` with its pin
`locks/pins/mica-core.pin`. The assembly, `micaoss/mica-build`, imports the
packages and builds none of them.

## Subprojects

Every crate lives in `crates/<package name>/`.

| Crate | Kind | What it is |
| --- | --- | --- |
| `micad` | binary `micad` | The management-plane daemon: the settings and live-state trees and the reconcilers (network, radios, containers, SSH/dropbear, updates), exposed on D-Bus as `com.mica.micad` at `/com/mica/micad` |
| `mica-apid` | binary `mica-apid` | The HTTPS API daemon and web dashboard; it changes the system only through micad's D-Bus interface. The UI is `crates/mica-apid/ui/` (React + Vite, built with Bun and embedded at compile time); the API is described by `crates/mica-apid/openapi.json` |
| `mica-mqttd` | binary | The MQTT application-data bridge for enrolled `com.mica.*` services |
| `mica-mqtt-broker` | binary | The local MQTT broker (rumqttd as a library), configured from the file micad renders |
| `mica-sftp-server` | binary | An SFTP version 3 server on stdin/stdout, run by dropbear as the logged-in user |
| `mica-deploy` | binaries `mica-deploy`, `mica-runkit` | Authenticated file deployments and updates (`mica-deploy`) and early boot and shutdown (`mica-runkit`) |
| `micad-settings` | library | The typed settings tree, its dot-path API and its persistence |
| `mica-busname` | library | The one rule turning a `com.mica.*` service name into the class shared by the service registry and MQTT addressing |
| `mica-ui-bundle` | library, tool `mica-ui-pack` | Validation, deterministic packing and safe extraction of `.mica-ui.zip` UI bundles |
| `lifecycle-sys` | library | Typed, bounded Linux device operations for boot and shutdown; the one crate allowed `unsafe` |
| `mica-mqtt-reference` | binary, not packaged | The small `com.mica.Item1` service that proves the MQTT application boundary on a test image |

## Layout

| Path | Contents |
| --- | --- |
| `crates/` | The workspace |
| `pkgs/<producer>/` | One package producer each (`micad`, `apid`, `mqtt`, `sftp`, `deploy`, `lifecycle`): `producer.env`, `Dockerfile`, control templates and the build hook; `pkgs/README.md` is the packaging contract |
| `scripts/build/` | Build entry points: the lock checker and reader, the image resolver, the producer compile, the offline build and the release publisher |
| `scripts/deb/` | The packer, the pool index and the package gate |
| `scripts/gate/` | The gates: Rust, UI build contract, D-Bus policy, boot/shutdown, IO faults, and the lock and publisher tests |
| `locks/` | The inputs: `mica-build-env.lock` (the lock asset of a `micaoss/mica-build-env` release, unchanged) with its pin `pins/mica-build-env.pin`, and `upstream.lock` |
| `scripts/gate/vectors.pin` | The mica commit the release-lock vectors are READ at; they are never copied here (`scripts/gate/vectors-source.sh` fetches them into the git-ignored `repos/` cache) |
| `docs/` | Tasks, plans and the changelog |

## Building

Everything compiles and packs inside the `rust` and `base` images
`locks/mica-build-env.lock` names; the host needs only docker, bash, curl and
jq, and carries no Rust toolchain.

```
make deps            # check locks/ and verify every pinned lock against its release
make check           # lint, lock and publisher tests, UI build contract, Rust gate, boot/shutdown fixtures, IO faults
make deb             # every producer for MICA_ARCH (amd64 or arm64) into _out/debs/<arch>/pool
make pool            # both architectures, indexed
make package-gate    # the package gate, including a byte-identical rebuild
```

`make help` lists every target. Build outputs go under `_out/`.

## CI and releases

- `ci.yml` runs on every push to `main` and every pull request (changes to
  docs or markdown alone excepted): the gates, then every package built, packed
  and gated for amd64 and arm64 on runners of that architecture, then both
  pools gated together. It publishes nothing.
- `release.yml` runs only when a release is published. A release is cut with
  `gh release create <YYYYMMDD-HHMM> --target <commit of main>`; the workflow
  builds that tag the same way, pushes both pools to `ghcr.io/micaoss/mica-core`
  and reads them back with no credential, then attaches `mica-core.lock` and
  `SHA256SUMS` and reads those back. A pushed tag and an attached asset are
  never replaced.

## License

Apache License 2.0; see `LICENSE`.
