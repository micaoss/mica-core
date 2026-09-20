# 20260920-0620-ssh-without-pam SSH authenticates without PAM, and nothing says it must

- **status**: in_progress
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:20

## Description

Console login aborts on every published product: `/etc/pam.d/login` is absent
and `/etc/pam.d/other` includes four `common-*` files that do not exist, so the
stack cannot be assembled and PAM returns `PAM_ABORT`. That defect is
mica-build's and mica-system-base's. What is **mica-core's** is the question it
raised: SSH still worked, so the only access path to every fielded device runs
on a property nobody chose.

## 1. Why dropbear does not need PAM -- measured, not inferred

From the exact artifact `mica-system-base:locks/upstream.lock` pins
(`dropbear-bin 2025.89-1~deb13u1`, arm64, sha256 `1cfab9f1…`, downloaded and
verified against that row):

- `Depends: libc6, libcrypt1, libtomcrypt1, libtommath1, zlib1g` -- no `libpam`
  of any kind.
- `/usr/sbin/dropbear` `NEEDED`: `libtomcrypt.so.1`, `libtommath.so.1`,
  `libz.so.1`, `libcrypt.so.1`, `libc.so.6`, `ld-linux-aarch64.so.1`.
  **`libpam.so.0` is absent.**

So it authenticates against `/etc/shadow` through `crypt(3)`. Upstream dropbear
can be built `--enable-pam`; Debian's package is not.

## 2. Is it intended? No

Nothing in mica-core or mica-system-base states that dropbear must not use PAM
-- both trees were grepped and neither mentions it outside an unrelated
`pam_unix` comment in `mica-system-base:debs/mica-system/postinst`. The PAM
libraries **are** pinned and installed (`libpam0g`, `libpam-modules`,
`libpam-runtime` in `locks/upstream.lock`; `packages.tsv` puts
`libpam-modules` in `base` and `mica-system`), which is why console login has
the libraries and not the stack. Dropbear avoiding PAM is Debian's packaging
default. **An accident that saved the fleet is still an accident.**

## 3. What should assert it, and where

Nothing does today. A rebuild that picked up `libpam` -- a flag, a build-env
dependency appearing, an upstream default moving -- would remove the only way
anyone can reach a device, with no other symptom and no gate failing.

The assertion is one line and belongs where the package is pinned and the root
is composed, which is **not** this repository:

- **mica-system-base** (primary): in `tests/debs.test.ts` or
  `tests/payload/access.test.ts`, assert that the pinned `dropbear-bin` names no
  `libpam*` in `Depends` **and** that `/usr/sbin/dropbear` in the built root has
  no `libpam*` in its dynamic dependencies. It pins the package and composes the
  root, so both facts are in reach there.
- **mica-build** (defence in depth): the composed-root gate can assert the same
  on the actual product root, where a late addition would still be caught.

mica-core cannot assert it -- it ships no dropbear -- but it **depends** on it:
`crates/micad/src/transient.rs` writes the root hash into `/etc/shadow` and
`crates/micad/src/reconciler/sshd.rs` renders dropbear's arguments and keys, and
both assume dropbear reads that file. The dependency is now stated in
`docs/design/apid.md` and in the source comments, so the assumption is written
where it is made.

## 4. Where the transient root password appears

Documented in `docs/design/apid.md`, *Reaching a device shell*: claim with
`POST /api/v1/setup`, then either install a public key
(`access.ssh.authorizedKeys`, rendered for `root` and `mica`, SSH off until
enabled) or `POST /api/v1/actions/transient-root-password` -- 8 to 72 bytes, no
NUL, newline or carriage return, **202** on success, **422** naming the bound
without repeating the password. In the UI it is the **Access** page,
*Transient root password*, "8-72 bytes; removed at the next reboot."

## ActiveForm

Recording why SSH works without PAM and proposing where that is asserted

## Dependencies

- The executable assertion is mica-system-base's and mica-build's to land; the
  coordinator routes it. This record carries the measurement they need.
