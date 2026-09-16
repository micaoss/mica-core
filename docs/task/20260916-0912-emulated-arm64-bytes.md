# 20260916-0912-emulated-arm64-bytes What differs between an emulated and a native arm64 archive

- **status**: pending
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-16 09:12

## Description

Building the arm64 pool on an amd64 workstation goes through
`docker buildx build --platform linux/arm64` (`scripts/deb/build.sh`), so it
runs under QEMU emulation, while CI builds arm64 natively on
`ubuntu-24.04-arm`. The two do not agree byte for byte: measured on
2026-09-16, all six packages at `0.1.0-1` differed from what release
`20260915-1135` published, while the same six rebuilt byte-identically on
amd64.

The difference is not the build-env images. The control is what makes that a
conclusion: rebuilding the arm64 pool on the same station with the old
`20260915-0138` lock produced exactly the bytes the new `20260916-0735` lock
produces, package for package, and both differ from the released ones.

| package | emulated here, both locks | released natively |
| --- | --- | --- |
| micad | 906ce7dd… | 13570a0f… |
| mica-deploy | 81b27604… | f804db20… |
| mica-lifecycle | 3c158c21… | d1c63c2f… |
| mica-mqtt-broker | 272f30b4… | e3e7788b… |
| mica-mqttd | afcc6e27… | 10b22be0… |
| mica-sftp-server | cb794168… | 0d63bf29… |

What is unknown is *what* inside the archive differs. The guard compares whole
archives, so the candidates are untested: a build id, a path recorded in the
binary, the ordering or timestamps of the archive members, or code generation
itself. Three of those four are fixable and the fourth is not, which is why
the answer decides whether anything should be done at all.

Consequences already recorded and forwarded (coordinator, 2026-09-16): a local
guard cannot validate an arm64 half on an amd64 station, and `make offline`
there produces arm64 archives that differ from the released ones -- which
bounds what offline means for this workspace.

Acceptance: name the difference for one package, by diffing the released arm64
archive of `20260915-1135` against an emulated rebuild (archive members first,
then the binary: build id, `.comment`, debug paths, then instruction bytes);
say which of the four candidates it is; and propose a fix, or record that it is
code generation and cannot be fixed here.

Not in scope: changing how CI builds, and any version bump. The emulated
archives are never published.

## ActiveForm

Naming what differs between an emulated and a native arm64 archive

## Dependencies

- Runs after the current round (the build-env `20260916-0735` move and the
  release carrying `mica-apid 0.1.0-2`), on the coordinator's instruction.
