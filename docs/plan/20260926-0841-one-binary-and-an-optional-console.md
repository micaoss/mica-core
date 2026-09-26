# 20260926-0841-one-binary-and-an-optional-console One management binary, an optional console, stripped executables

- **status**: implementing
- **approvedAt**: 2026-09-26 (user: "开工")
- **createdAt**: 2026-09-26 08:41
- **relatedTask**: 20260926-0841-one-binary-and-an-optional-console

## Context

Measured on the released `20260926-0815` pool (amd64):

| File | Bytes | Of which |
| --- | --- | --- |
| `mica-apid` `/usr/bin/mica-apid` | 17 297 416 | `.text` 8.9 MB; `.strtab` + `.symtab` 3.9 MB; `.rodata` 2.2 MB, of it the built-in console 1.3 MB (61 files, embedded raw) |
| `micad` `/usr/bin/micad` | 11 413 120 | unstripped likewise |
| `mica-apid` `/usr/share/mica-apid/openapi.json` | 307 923 | |

- **Nothing but `mica-runkit` is stripped.** `scripts/build/build-deb.sh` passes `strip=symbols` to
  the static runkit build only; the workspace has no `[profile.release]`. Every other executable
  ships its full symbol table.
- **The console is compiled into the API.** `crates/mica-apid/build.rs` refuses to build without
  `MICA_APID_UI_DIST_DIR` and embeds every file, so a product that wants the API and not the console
  still carries it, and building apid needs the Bun toolchain first.
- **micad and apid are two binaries that link the same stack**: tokio, zbus, serde, the
  settings crate, tracing, argon2, rustix. They are already one version: `mica-apid` depends on
  `micad (= <version>)` because both speak `com.mica.micad1` from one commit.

The split into two executables (changelog 2026-09-13 21:20; `docs/design/apid.md` 8) was made
"so an API upgrade never repacks micad". The exact-version dependency means that has never been
possible, and the independent-upgrade plan it anticipated was only ever a proposal. This plan
reverses that decision and says why here, rather than implying it.

## Proposal

Three changes, in this order because each is measurable on its own.

### 1. Strip every shipped executable

`[profile.release] strip = "symbols"` in the workspace `Cargo.toml`, which covers every producer;
runkit's explicit flag stays redundant and harmless. Behaviour is unchanged. What is lost is
symbol names in a panic's backtrace on a device; the unstripped build stays reproducible from the
same commit, which is where a backtrace is resolved.

Not in this plan: `lto`, `codegen-units = 1`, `opt-level = "s"` (worth measuring, and they cost
build time) and `panic = "abort"` (changes what a panic does).

### 2. The console becomes a package: `mica-apid-ui`

- apid no longer embeds the console. `build.rs` and its `MICA_APID_UI_DIST_DIR` requirement go;
  building apid needs no Bun toolchain.
- The built console is installed by a new package, `mica-apid-ui`, at `/usr/share/mica-apid/ui`,
  inside the dm-verity root, so it is as authenticated as it was inside the binary.
- apid serves `/_ui` from that directory when it exists, with the same path rules the embedded
  resolver applies; without it `/_ui` and `/` answer 404 and the API is untouched. The operator
  bundle mechanism (`/v1/ui/*`, `/mica/ui`) is unchanged.
- A product chooses the console by including the package: an API-only product omits it. No
  feature flag, one `openapi.json`, one test matrix.

### 3. One executable: `mica-apid` is `micad` under another name

- `micad` becomes a multi-call binary: invoked as `mica-apid` (its `argv[0]` basename) it runs
  apid's entry point, otherwise micad's. `/usr/bin/mica-apid` is a symlink to `micad`.
  `apid.service`, its sandboxing and its user are unchanged: what separates the two daemons is
  their units, not their files, and apid already runs as root.
- The `micad` producer ships all three packages from one build: `micad` (the binary), `mica-apid`
  (the symlink, `apid.service`, `openapi.json`; `Depends: micad (= <version>)`) and
  `mica-apid-ui`. `pkgs/apid` goes.
- The `mica-apid` crate keeps its library and its own `mica-apid` bin target for development,
  the end-to-end tests and `--openapi`; no producer ships that target.

### Versions

The three packages share the producer's version, and a package version never goes backwards:
`mica-apid` is released at `0.1.0-4`, so the producer takes `0.1.0-5` for all three (`micad`
`0.1.0-3` becomes `0.1.0-5`). The mqtt producer pins `micad (= 0.1.0-5)` and moves to `0.1.0-4`.
`check.sh`'s rule, "a producer's upstream version is its binaries' crate version", still holds:
both crates are `0.1.0`.

### Order of work

1. Strip; measure every package against `20260926-0815`.
2. Console out of the binary into `mica-apid-ui`; apid serves it from disk; tests for present,
   absent and the path rules.
3. Multi-call `micad`; merge the apid producer; the package gate checks the symlink and that
   `mica-apid --version` and `--openapi` answer through it.
4. Measure again; docs (`apid.md` 8 rewritten, `architecture.md`, `packaging-and-release.md`),
   changelog, release.

## Risks

- **mica-build must add `mica-apid-ui`** to every product that wants the console, in the commit
  that pins this release; a product that pins it and forgets gets an API with no console, not a
  broken device. The package set is otherwise unchanged (`micad`, `mica-apid` keep their names).
- **One binary for a network-facing and a bus-facing daemon.** The privilege boundary was never
  the file: apid runs as root under `ProtectSystem=strict`, micad as root with the bus. Both
  units keep their own sandboxing; the code apid can reach in its process is the code it links.
- **Backtraces lose names** on devices (item 1).
- **Reversing a recorded decision** (2026-09-13): this plan is the record, and `apid.md` 8 is
  rewritten to say what holds now.

## Scope

In: the workspace profile, `crates/mica-apid` (build script, asset serving), `crates/micad`
(entry point), `pkgs/micad`, `pkgs/apid` (removed), `pkgs/mqtt` (pin), the package gate, the
build scripts that fed the console into the apid build, docs. Out: mica-build's product
definitions (the counterpart), the console itself, the operator bundle API, the MQTT and SFTP
daemons (candidates for the same multi-call binary later, measured separately).

## Alternatives

- **A `ui` cargo feature and a headless apid producer** (option A of the discussion): also drops
  the bundle code, at the cost of two feature sets to test, two OpenAPI documents and a second
  producer for one crate. Rejected by the user in favour of this.
- **Keep two binaries, strip only.** Leaves the shared stack linked twice.

## Annotations

- 2026-09-26 (user): "用方案b，然后strip也做，然后看看再发布一个类似用micad ln一个apid这样的形式，
  这样也可以节省空间，后续只需要一个micad就可以了".
- 2026-09-26 (implementation): measured on amd64 against `20260926-0815`, installed bytes. Strip
  alone: `mica-apid` 17 297 416 to 13 394 560, `micad` 11 413 120 to 8 751 728. All three: the one
  `micad` executable is 18 058 528 (it carries apid, without the console and without symbols);
  `mica-apid` is a symlink, 34 844 bytes of archive; `mica-apid-ui` 1 312 046 installed. Totals
  29.0 MB before, 19.7 MB with the console and 18.4 MB API-only after.
- 2026-09-26 (implementation): **every producer moves**, not only the three: the workspace
  profile is an input to all of them and strips their binaries, so `mica-deploy` and
  `mica-lifecycle` go to `0.1.0-6` and `mica-sftp-server` to `0.1.0-2`, beside `0.1.0-5` for the
  micad producer and `0.1.0-4` for the mqtt one.
