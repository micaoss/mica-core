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
| `cpu` | nothing | create a bound | a booted guest showing `cpu.max` honoured -- **measured, same boot** |

**So omitting a field means two different things in one struct: an absent
`pids` is 2048 and an absent `memory` is unlimited.** Nothing in the type says
so, so the field documentation must, and the console must not present the three
as one list of empty boxes.

## Why a resource waits for a controller

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

### The gate is discharged for all three, on one product (2026-09-21)

`mica-build` booted **uefi-x64-prod** (Mica OS `0.1.0+git940f870cc632-1`, board
`uefi-x64.20260920-1536`, kernel 6.12.107) and read the device rather than its
configuration:

- `cgroup.controllers` carries `cpuset cpu io memory pids`, and
  `cgroup.subtree_control` carries `memory pids` -- **so the memory controller
  is not only compiled in, it is delegated**, which a kernel config cannot say.
  `cpu` is in `controllers` and **not** in the root's `subtree_control`, which
  this record held for a day as "available and not delegated". **It is not that,
  and the second run says so.**
- `podman run --memory=64m` gave `memory.max = 67108864` inside the container.
- **`podman run --cpus=0.5` gave `cpu.max = 50000 100000`**, read inside the
  container and again on the scope from outside (`cpu.weight = 100`), and the
  scope's own `cgroup.controllers` carried `cpu` -- which happens only if the
  chain above it enabled it. **So `cpu` is delegated where and when something
  asks, and handed back after**: the root's `subtree_control` read `memory pids`
  both before and after. `cpu` and `memory` behave identically; what differs is
  only what a bare `podman run` gets when nobody asks -- pids 2048 by engine
  default, memory and cpu unbounded.
- Two limits of that reading, stated rather than implied: the root was read
  **before and after, not during**, so "the root's list changed and changed
  back" is the only reading consistent with the scope's measurements and is an
  **inference**; and the accounting-defaults explanation (memory and tasks on by
  default, CPU off) is a **hypothesis** -- `DefaultCPUAccounting` was not read.
  Neither affects the field: `cpu.max` inside the container is a measurement.

**And the day this record spent holding `cpu` back came from reading a root
cgroup file as a standing property when it is a snapshot of one instant on an
idle system.** The root's `subtree_control` says what is delegated *now*, not
what can be. This is the third costume of the same root-cgroup error in two
days, after `cpu.max` at the root (which can never exist) and `cpu` listed in
`controllers` without `CFS_BANDWIDTH` being implied. **Nothing in the file says
which kind of thing it is, so the reader has to.**
- And on the path this proposal's field actually travels: a `.container`
  carrying **no resource key at all**, rendered by the shipped Quadlet, with no
  `--pids-limit` in its `ExecStart` -- and the payload child at
  `pids.max = 2048`, surviving `--cgroups=split`. **An absent `pids` is 2048,
  measured on a device rather than read out of podman's source.**
- The same payload showed `memory.max = max`: **a rendered container today is
  bounded in pids and unbounded in memory and cpu, on a kernel that bounds all
  three when asked.** That is the gap this proposal closes, now
  measured rather than inferred.

**Scope, stated rather than assumed: that is one product.** All three resources
are now measured on it; the other three boards have the config census above -- the kernel *can* -- and not a boot. The
fields land on all four anyway, for reasons that are about the failure mode
rather than about optimism:

- The half a boot adds over a config is **delegation**, and delegation is
  `systemd`'s, from the same `mica-system-base` root on every product. The
  variable the census cannot see is the one thing that does not vary by board.
- If the assumption is wrong the container **refuses to start**, naming the
  limit, with the unit in `failed`. That is loud, attributable and reversible by
  removing the field -- not a silent unbounded container.
- Acceptance therefore carries one addition: **read `cgroup.subtree_control` on
  a FIT board the next time one is booted for any reason.** It is a line of
  output on a boot that is happening anyway, not a round of work.

**The board argument and the resource question are different axes, and only the
resource one moved.** `cpu` was held back for a day not because the board
argument was weaker for it but because there was nothing to extrapolate from;
one command on a guest that was already up supplied the starting point, and the
`cpu` row discharged without a single other sentence in this record needing to
be re-argued. The board line is untouched: **the run is `uefi-x64-prod`**, the
other three rest on config plus identical delegation, and the one-line
acceptance above still stands.

## Proposal

1. **`pids` lands unconditionally.** `Option<u32>`, rendered as `PidsLimit=` in
   the `.container` unit. Absent means "podman's default", which is 2048 and is
   documented as such rather than as "no limit".
2. **`memory` and `cpu` land together on the 2026-09-21 boot**, as
   `Option<String>` each (a byte size rendered as `Memory=`, a quota/period pair
   rendered as `CPUQuota=`), refused by `validate_container_units` when the
   syntax is not what systemd accepts. **One decision, not two**: the guest
   honoured both when asked.
3. **The reconciler does not probe.** A device whose kernel lacks the controller
   and whose unit declares a limit fails at `crun`, loudly, with the unit in
   `failed` -- which is the observable failure. micad does not silently drop a
   declared limit: a dropped limit is a bound the operator believes in and does
   not have.

## Acceptance

`pids` declarable, rendered, and defaulted-documented; `memory` and `cpu`
implemented on the boot evidence recorded above; the asymmetric default stated
in the type's documentation, in `docs/design/` and in the console; and a test
that a unit declaring no limits renders exactly the file it renders today.

## And the bind is meant to be inactive

The same run nearly produced a bug report against this repository: two attempts
found `/etc/containers/systemd` empty. That is the designed state --
`mica-build`'s own check **refuses a statically enabled bind**, because a bind
up on every boot lets anything able to write `/mnt/data/state/quadlet` obtain a
root-capable container at the next reboot with no operator decision in the path.
`ContainerReconciler::turn_on` -- enable the mount, start it, render, reload,
start the units -- is the only sanctioned way in, and the third attempt took it.
**The empty directory is the security property, not a broken rendering path.**

## Not in scope

Probing the kernel to decide whether to render a limit, and any attempt to make
a missing controller silent.
