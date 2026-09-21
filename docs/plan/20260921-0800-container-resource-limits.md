# 20260921-0800-container-resource-limits Resource limits on a declared container

- **status**: proposed
- **createdAt**: 2026-09-21 08:00
- **relatedTask**: (none yet)

## Context

`ContainerUnit` (`crates/micad/src/reconciler/container.rs`, the type
`20260920-0639-container-management` introduced) carries image, command,
environment, published ports, volumes, restart policy and `autoStart`. **No
resource limit is declarable in a unit.** That sentence is about the type, not
about the device: podman imposes a pids ceiling whether anybody asks or not, so
a device is not unbounded in every dimension.

## The three resources are three different stories

A proposal that listed them together would be wrong about the first.

| resource | today | a field would | precondition |
| --- | --- | --- | --- |
| `pids` | **2048 on every container**, set by podman's `DefaultPidsLimit`; `mica-podman`'s `containers.conf` does not override it | **override** an existing ceiling | none |
| `memory` | nothing | **create** a bound where none exists | a booted guest showing `memory.max` |
| `cpu` | nothing | create a bound | a booted guest showing the `cpu.max` interface |

**So omitting a field means two different things in one struct: an absent
`pids` is 2048 and an absent `memory` is unlimited.** Nothing in the type says
so, so the field documentation must, and the console must not present the three
as one list of empty boxes.

## Why `memory` and `cpu` wait

Setting a limit the kernel cannot enforce does not leave the container
unbounded -- **it makes the container refuse to start**, because the write fails
in `crun`. A declared limit that prevents the container from running is a
regression dressed as a feature. So the controllers must be present before the
fields exist.

### The census: present on every board mica-build pins today

Read 2026-09-21, at the releases `mica-build:locks/pins/mica-boards.<board>.pin`
names **at origin** (`20260920-1536`, all four), each board read the way that
board must be read:

| board | source | `MEMCG` | `CFS_BANDWIDTH` | `CGROUP_PIDS` |
| --- | --- | --- | --- | --- |
| uefi-x64 | committed `kernel/config` at the tag | y | y | y |
| uefi-arm64 | committed `kernel/config` at the tag | y | y | y |
| cx3576 dev | published `kernel.cx3576.20260920-1536`, `kernel/dev/config`, by digest | y | y | y |
| cx3576 prod | same component, `kernel/prod/config` | y | y | y |
| s905x5m dev | published `kernel.s905x5m.20260920-1536`, `kernel/dev/config`, by digest | y | y | y |
| s905x5m prod | same component, `kernel/prod/config` | y | y | y |

Three things about *how* that was read, because each is a way to get a clean
answer about the wrong artefact:

- **On a FIT board the committed `kernel/config` is a vendor input, not the
  recorded output**, so the published kernel component is the only source that
  answers about that board's kernel (`mica-boards`, 2026-09-20).
- **`dev` and `prod` are separate kernels on the FIT boards** -- different
  config blob digests, one byte apart in length -- so one profile is not
  evidence about the other. Six readings for four boards, not four.
- **The pins were read at `origin`, not from the workspace checkout**, which
  held `20260916-0857` for three boards and `20260917-1007` for the fourth: a
  set that disagrees looks like a real state of the world, and a census run
  against it would have reported a clean result about the wrong releases.

**And a config symbol is still the wrong kind of statement to land on.** It says
the kernel *can*; a file in `/sys/fs/cgroup` says the device *does*. The
measurement this replaces was a boot, so a boot replaces it.

## Proposal

1. **`pids` lands unconditionally.** `Option<u32>`, rendered as `PidsLimit=` in
   the `.container` unit. Absent means "podman's default", which is 2048 and is
   documented as such rather than as "no limit".
2. **`memory` and `cpu` land after a booted guest shows the controller
   files**, as `Option<String>` (a byte size and a quota/period pair rendered as
   `Memory=` and `CPUQuota=`), refused by `validate_container_units` when the
   syntax is not what systemd accepts.
3. **The reconciler does not probe.** A device whose kernel lacks the controller
   and whose unit declares a limit fails at `crun`, loudly, with the unit in
   `failed` -- which is the observable failure. micad does not silently drop a
   declared limit: a dropped limit is a bound the operator believes in and does
   not have.

## Acceptance

`pids` declarable, rendered, and defaulted-documented; `memory` and `cpu`
implemented only after the boot evidence exists; the asymmetric default stated
in the type's documentation, in `docs/design/` and in the console; and a test
that a unit declaring no limits renders exactly the file it renders today.

## Not in scope

Probing the kernel to decide whether to render a limit, and any attempt to make
a missing controller silent.
