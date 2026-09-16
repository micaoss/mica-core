# 20260916-0912-cross-built-arm64-bytes What differs between a cross-built and a native arm64 archive

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-16 09:12

## Description

`scripts/build/build-deb.sh` runs the rust image on the **host** architecture
and cross-compiles to the target (`--target aarch64-unknown-linux-gnu`, linked
with `aarch64-linux-gnu-gcc` per `.cargo/config.toml`). On an x86-64 station the
arm64 archives are therefore cross-built; in CI the arm64 job runs on
`ubuntu-24.04-arm`, where the same command is a native build. Nothing here is
emulated -- the first framing of this, "emulated arm64 does not reproduce native
arm64", was wrong about the mechanism, and the correction matters because it
explains why `mica-system-base` and `mica-podman` reproduce (they run the
container **on** the target platform, so their only difference is emulation,
which does reproduce) while mica-core does not.

Measured 2026-09-16 against release `20260915-1135`, on build-env
`20260916-0735`: all six packages at `0.1.0-1` rebuilt byte-identically on
amd64, and all six differed on arm64. The build-env is not the cause; the same
station with the old `20260915-0138` lock produces exactly the same arm64 bytes.

What the diff of `micad` arm64 (released `13570a0f…`, local `906ce7dd…`) found:

- The `.deb` differs in exactly one file, `usr/bin/micad`. `control`
  (`Installed-Size`) and `md5sums` follow from it. Member names, order, modes,
  owners and mtimes are identical: not archive ordering, not timestamps.
- The local binary carries a section the released one does not have at all,
  `.note.package`, 116 bytes:
  `{"type":"deb","os":"debian","name":"cross-toolchain-base","version":"77","architecture":"amd64"}`.
  The linker names itself as the amd64 cross toolchain.
- Rust symbols differ in their crate disambiguators (`Cs20HYpRTzSG0_4zbus`
  against `CsrAm86bgweg_4zbus`): cargo computed a different `-C metadata`,
  because the rustc host triple is part of it. That renames every mangled
  symbol and moves `.symtab` and `.strtab`.
- `.comment` is identical in both: rustc 1.98.0 (88d9e12ae 2026-08-18), clang
  22.1.0-rc2, GCC (Debian 14.2.0-19) 14.2.0. The compilers are not the
  difference.
- With the disambiguator and the LLVM suffixes normalised, both binaries hold
  12662 functions and **no function has a different size**. What differs is the
  multiplicity of duplicated `drop_glue` instances and the number of linker
  erratum stubs (`e843419@…`: 8 released, 5 local), which are link-time, not
  code generation.

## Answer (2026-09-16)

**Stamps and link inputs, with no evidence of a code-generation difference.**
The control is two local cross builds of `micad` that differ only in
`-C metadata` -- same station, same image, same toolchain, same source -- and
they already reproduce every signal the cross-against-native comparison shows:

| | cross local vs native released | two local builds, only `-C metadata` differs |
| --- | --- | --- |
| `.text` | 5106740 vs 5094644 (-12096) | 5093188 vs 5093892 (+704) |
| functions | 12662 vs 12662 | 12664 vs 12666 |
| names on one side only | 253 / 255 | 60 / 64 |
| shared names, different multiplicity | 16 | 22 |
| of those, a different size | 5 | 12 |
| linker erratum stubs | 8 vs 5 | 4 vs 5 |
| `.note.package` | absent vs present | present in both |

The disambiguator alone moves `.text`, changes function counts, duplicates
`drop_glue` differently and moves the linker's erratum stubs -- by the
different-size metric it is noisier (12) than the comparison under
investigation (5). The one artifact unique to cross is `.note.package`, the
cross linker naming itself.

Residual, stated rather than hidden: the `.text` delta of the real comparison
is 17 times the control's, so something beyond naming contributes, most
plausibly different `crt` and `libgcc` objects out of the cross package. That
is a link input, not code generation. Proving the machine code identical
byte-for-byte is not possible while the disambiguator perturbs every symbol, so
the claim is bounded: cross versus native provides **no additional evidence**
of code generation divergence beyond what changing one metadata string does.

## Decisions (coordinator, 2026-09-16)

- Building the local pool on the target platform (host equal target
  everywhere, as `mica-system-base` and `mica-podman` do) is the **known** fix,
  not a speculative one: `mica-podman` builds netavark and aardvark-dns with
  `cargo build --release` in a container run on the target platform, and its
  arm64 package built under emulation on this station hashes
  `b7f23a277a4d3204b6d1551fe0bad5bca8d2aee5c31f413a2d5a977324606de3`, exactly
  the row its native release `20260916-0846` published. Rust does reproduce
  under emulation.
- It is still not taken now, for the honest reason: it changes not one shipped
  byte, because the published archives are already the native ones, and the
  seven version bumps `build-deb.sh` being in the inputs hash forces are a bad
  trade for that today. **Take it at the next round in which these packages
  bump for another reason**; if no such round arrives in reasonable time,
  propose it deliberately (coordinator, 2026-09-16).
- The `-C metadata` control below says the residue is stamps and link inputs,
  so a cheaper fix than moving the whole build may exist: pinning what the
  linker stamps. Not investigated; it would have to survive the same
  measurement.
- Making CI cross-build arm64 is **refused**: it would make the two agree by
  lowering the published artifact to the local one.
- The bound is stated instead, in `docs/development.md`: the published archives
  are the native ones, a local arm64 archive is not one of them, and CI is the
  authority for the arm64 half.

## ActiveForm

Separating stamps from code generation in the cross-built arm64 binary

## Dependencies

None. It reads published artifacts and builds locally; it blocks nothing.
