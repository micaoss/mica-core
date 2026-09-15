# 20260915-1059-package-versions Packages locked by their declared version

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-15 10:59

## Description

User decision 2026-09-15 (`mica:docs/decisions/2026-09-15-package-versions.md`,
R0-R8; release lock spec at mica `1266fbf`): a package is locked by its own
declared version, a release never changes it, and an unchanged package is
reused from the previous release under the rules.

- R1/R2: `pkgs/<producer>/producer.env` declares `VERSION` (initially
  `0.1.0-1`; the upstream part is the crate version of its binaries,
  asserted by `scripts/build/check.sh`) and `SOURCE_DATE_EPOCH`; the `VERSION`
  file and `scripts/deb/version.sh` are removed. `micad (= 0.1.0-1)` is written
  literally in its dependents (R7) and checked by `scripts/deb/producers.sh`.
- R3: no `Mica-Source-Commit`, no `MICA_BUILD_COMMIT`; `--version` prints the
  package version and micad's system information drops the commit, the git
  stamp and the commit date (apid's diagnostics schema follows).
- R4: `scripts/deb/inputs.sh`, recorded in `_out/debs/<arch>/inputs.tsv` and as
  each pool layer's `mica.inputs`.
- R5: `scripts/build/reuse.sh` against the newest release whose pool layers carry
  `mica.inputs`, in `ci.yml` (read-only) and `scripts/build/release.sh`.
- R6: pool manifests carry only `mica.source-repo` and `mica.arch`.
- R8: `make offline` warns that the guard did not run.
- Combined (user, 2026-09-15): `mica/rootfs/v2` drops the rootfs `version`, so a
  root's id depends only on its content; the contract fixtures are regenerated.
  micad reads no `release-identity.env`.

Acceptance: `make check`, `make pool` and the package gate pass; pushed with CI
green; one release under the rules (a full build recording `mica.inputs`),
verified anonymously and reported.

## ActiveForm

Locking packages by their declared version

## Dependencies

- mica-build: stops reading `Mica-Source-Commit` and `+git` versions and follows
  the new `--version` lines before pinning this release.
