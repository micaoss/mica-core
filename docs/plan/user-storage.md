# User storage (r3): no apps, /srv is the user's, the user mounts their own disks

Status: approved plan (storage-20261111, r3 with the user's answers). Round R is done (core 8323275 removed the apps feature; Base 8a8e7ee and bfdf0cc); U2 and U3 are not. r1 (system-managed locations) was rejected; r2's app-data move is dropped because the apps feature goes.

The direction, as in Fedora CoreOS: users run what they want with their own units (`mica-unit`) or containers, and mount their own disks. The system does not treat `/srv` as special, and does not manage apps or user disks.

Facts below are from mica-core main 6360fdf, mica-system-base main 510f4ec, mica-build, mica-pkgs and mica-podman (read only).

The user's answers to r2 are built in:

- the application-data reset stops clearing `/srv`;
- a user may mount at `/srv` itself;
- OpenRC gets `/usr/local/etc/fstab` with `mica-fstab`;
- `.swap` and `.automount` are allowed, with nothing enabled by default;
- `/srv` and `/home` get `nosuid,nodev`.

## 1. Removing the apps feature

### 1.1 What goes

**micad** (crates/micad):

- `src/apps.rs` and `src/apps/{bundle,devices,flow,manifest,render,seed,update,tests}.rs`: bundles, the manifest, privilege and device-class rendering, `setpriv` wrapping, seeds, update checks, first-run.
- `src/bus/apps.rs` (`app_upkeep`, the app tasks).
- The bus members in `src/bus/interface.rs`: `GetApps`, `GetApp`, `InstallApp`, `RollbackApp`, `RemoveApp`, `CheckAppUpdate`, `UpdateApp`, `StartApp`, `StopApp`, `RestartApp`, `GetAppLog`. Also `AppFault` in `bus/errors.rs`, and the introspection XML in `tests/introspection`.
- `src/app_cli.rs` and `micad app` in `main.rs`.
- The wiring in `lib.rs` (`with_apps`, `app_upkeep`) and `bus.rs` / `bus/builder.rs` (the `apps` field).
- The mDNS hook `reconciler::mdns::AppEntries` (`reconciler/mod.rs`, `mdns.rs`). The general mechanism, `/run/mica/mdns.d` drop-ins (3.13), stays and is how a user's unit publishes.
- In the user-units listing, the "unit belongs to an app" marking (`bus/tests/units.rs` `an_apps_unit_is_its_own`).
- Reset: the `seeded-apps.json` record handling, and `/mica/apps` in the application-data tier's scope.
- Crate dependencies used only by apps (tar/gzip unpacking, the update fetch): found with `cargo machete` after the removal.

**mica-apid** (crates/mica-apid):

- `src/routes/apps.rs` with `GET/POST /api/v1/apps`, `GET/DELETE /api/v1/apps/{name}`, `POST /api/v1/apps/{name}/{action}`, `GET /api/v1/apps/{name}/logs`.
- `V1_APP*` and `APP_UPLOADS_DIR` (`/mica/apps/.incoming`) in `routes/paths.rs`, `routes/sandbox_tests.rs` and the apid unit's write path for it.
- The `AppFault` mapping in `routes/error.rs`.
- `src/tests/apps.rs`, the app parts of `settings_api/fake.rs`.
- `openapi.json`, regenerated.

**Console** (crates/mica-apid/ui):

- `features/services/{apps-panel,app-detail}.tsx` (+ tests) and `apps.ts`.
- The route `routes/services_.apps.$name.tsx` and `routeTree.gen.ts`.
- The apps section of the services page.
- The `apps.*` i18n keys (en, zh-CN), and the apps steps in `e2e/console.spec.ts`.

**Docs:**

- `docs/mica-core.md` §3.11 entirely, and the app references in §3.10, §3.13 and §4.
- `docs/apps/` (tailscale bundle, `pack.sh`, `first-run`). Base's `floor-and-options.md` already shows tailscaled as a plain `mica-unit` service, which is the replacement.

**Base** (mica-system-base):

- `mica-data-layout` stops creating `/mica/apps` (an existing one is left).
- `mica-accounts`' app class (`add`/`remove`/`list`, uids 30000-39999) becomes the **service account** class for users' own units (1.4). Its comments and docs stop naming apps.

**mica-build:**

- `tests/suites/lifecycle-uefi/reset-runtime.sh` writes and checks `/mica/apps/reset-content`. It moves to `/mica/volumes/reset-content`, and its `/srv` expectation flips: the application-data reset no longer clears `/srv` (section 2).
- No board or product seeds an app (`/usr/share/mica/apps` appears nowhere), so no image changes.

### 1.2 What stays, because something else uses it

- `mica-unit` and micad's user units (`services.units`): the replacement.
- `mica-accounts` (`build`, the login class): mica-gce-agent keeps metadata users with `add-login`/`list-login`; `build` runs at every boot.
- `/run/mica/mdns.d`: the drop-ins a user's unit uses to publish mDNS.
- `setpriv` itself (util-linux, in the root): only the apps' rendering used it in core; a user's unit may call it.
- Device classes and the capability allowlist do not stay (1.4).

### 1.3 No compatibility handling

No device has apps installed, and no image seeds one. The feature is removed outright: there is no upgrade step, no notice, no preservation of `/mica/apps/apps.json` and no rollback path. The application-data reset keeps clearing DATA/state `accounts`, which now holds the service accounts (1.4), as it clears the user's units they belong to. It no longer clears `/srv` (section 2).

### 1.4 Anything worth moving to mica-unit?

**No. Do not expand mica-unit.** A user's unit declares its own confinement in the init's own terms:

- under systemd: `User=`, `SupplementaryGroups=dialout`, `DeviceAllow=char-ttyUSB rw`, `CapabilityBoundingSet=`, `AmbientCapabilities=`;
- under OpenRC: `command_user`, `capabilities` (OpenRC >= 0.45).

The docs gain one short example per init (a serial-port daemon as a service account), in Base's `floor-and-options.md`. A non-root account for a user's unit comes from `mica-accounts add <name> <uid>`, the **service account** class (uids 30000-39999, decided): the existing app class, renamed, not expanded.

### 1.5 Order

One round, **R**, right after net-r1 and before anything else builds on apps:

- core: the removal;
- Base: `mica-data-layout`, and the `mica-accounts` service-account class;
- mica-build: `reset-runtime.sh`.

It also carries section 2's reset change.

## 2. /srv is purely the user's

- **The default stays a bind** of DATA's `/mnt/data/srv`: a plain writable persistent directory.
  - It becomes `private` (systemd `srv.mount` `Options=bind,private,nosuid,nodev`; OpenRC `mica-mounts` the same), so a user mount under `/srv` no longer propagates into `/mnt/data/srv`, where it would trip the reset's nested-mount guard.
- **The application-data reset stops clearing `/srv`** (agreed).
- **A factory reset wipes DATA**, so DATA's `srv/` goes with it. It runs in the runkit before the root starts, so no user mount exists then: **the system never touches a user disk.**
- **The user may mount at `/srv` itself** (agreed). The mechanism is section 3.3.
  - Under systemd, a mount unit's name is its path, so a user's `srv.mount` cannot stack over Base's `srv.mount`. A declared user mount at `/srv` **replaces** the default bind on both inits, for consistency.
  - DATA's `srv/` is then not mounted anywhere and is kept as it is. Removing the user's mount brings it back at the next boot.
- **Afterwards nothing in core or Base reads, writes, creates or clears anything under `/srv`**, except Base's mountpoint and default bind. After the apps removal, no system path points into `/srv`.
- **Docs.** `cd /srv/project && micad ctr up` and a tailscaled in `/srv/tailscaled` stay as examples of user choices.

## 3. The user's own mounts

### 3.1 What works today

| | systemd | OpenRC |
| --- | --- | --- |
| Persistent unit area | `/usr/local/lib/systemd/system` = bind of `DATA/state/systemd-units` | `/usr/local/etc` (`init.d`, `conf.d`, `enabled`) = bind of `DATA/state/openrc-local` |
| Tool | `mica-unit install/enable/disable/remove/list` | the same |
| Types | `.service .socket .timer .path .target .mount` | openrc-run scripts |
| Loaded | `mica-load-extensions.service`: `daemon-reload`, then `multi-user.target` again, once the bind is up | `mica-local` starts the enabled scripts |
| micad | `services.units."x.mount"` is accepted | scripts |

So a persistent user `.mount` works today under systemd, but **late**: after the engines may already have started (`mica-units.service`, which starts the engine, has no ordering against `mica-load-extensions`).

### 3.2 What is added

1. **`.automount` and `.swap`** in `mica-unit` and `services.units`.
   - Swap is the user's choice: a partition, or a swap file on DATA or a user disk. Nothing is enabled by default.
2. **systemd: user mounts load early.** A Base generator, `mica-user-mounts-generator`, runs at every daemon start and reload.
   - DATA is already mounted by the runkit before PID 1, so the generator reads the user's `*.mount`, `*.automount` and `*.swap` files straight from `/mnt/data/state/systemd-units`.
   - It emits the allowed ones (3.4), with their enablement links, into its output directory.
   - The user's mounts are then in the boot's first transaction, before `local-fs.target` completes and before any engine. This fixes the ordering without depending on `mica-load-extensions`.
   - For a user `srv.mount` only, it writes to `generator.early`, which outranks `/etc`. Base's `srv.mount` is then replaced (2).
   - `mica-units.service` also gets `After=mica-load-extensions.service`, for the user's non-mount units that services rely on.
3. **OpenRC: `/usr/local/etc/fstab`** (agreed), standard fstab syntax, applied by a new Base service, `mica-fstab`:
   - it validates every line (3.4), then mounts in file order;
   - `nofail` means a failure is logged and not fatal;
   - at stop it unmounts in reverse;
   - it runs after `mica-mounts` and before `mica-local`, `mica-units` and the engines;
   - a line for `/srv` makes `mica-mounts` skip the default `/srv` bind (2).
   - `mica-unit fstab check` validates the file. micad does not manage the file in this round.
4. Under systemd there is no user fstab: users write units, as with FCOS and Butane.

### 3.3 Example

```ini
# srv.mount: the whole of /srv on the user's disk
[Unit]
Description=/srv on my data disk
[Mount]
What=/dev/disk/by-uuid/1b2c...
Where=/srv
Type=ext4
Options=nosuid,nodev,noatime
[Install]
WantedBy=local-fs.target
```

`mica-unit install srv.mount && mica-unit enable srv.mount`, then reboot. On OpenRC, the same as an fstab line:

```
UUID=1b2c... /srv ext4 nosuid,nodev,noatime 0 2
```

### 3.4 Where a user may mount

`mica-unit` (from the unit's unescaped `Where=`), the generator and `mica-fstab` all check this list.

| Allowed | Why |
| --- | --- |
| `/srv` and `/srv/<name>/...` | the user's tree; `/srv` itself replaces the default bind |
| `/var/mnt/<name>/...` | FCOS' convention. Base creates `/var/mnt` (DATA var) |
| `/home/<user>/...` | a user's home |
| `/mica/containers/storage` (podman) or `/mica/containers/docker` (docker) | exactly the engine's root (section 4) |
| swap: any block device by UUID or label, or a file on an allowed tree or on DATA | the user's choice |

| Refused | Why |
| --- | --- |
| `/`, `/usr/**`, `/etc/**`, `/boot/**`, `/proc`, `/sys`, `/dev`, `/run/**` | image, overlays, kernel |
| `/mnt/**` | `/mnt` is on the read-only root, and `/mnt/data` is DATA |
| `/mica` and everything under it except the two engine roots | system data, config, updates, UI |
| `/var/lib/mica/**`, `/var/log/**`, `/var/lib/<image service>/**` | STATE and system state |
| `/root`, `/tmp`, `/var/tmp`, `/home` itself | not storage locations |

A refusal is `mica-unit: refused mountpoint <path>` (exit 1), the same contract as a refused unit name. Mount options are the user's; the docs recommend `nosuid,nodev`.

## 4. Container storage

**Recommendation: the user mounts their disk at the engine's root**: `/mica/containers/storage` (podman `graphroot`) or `/mica/containers/docker` (docker `data-root`). There is no engine setting, which would be a system-managed knob. The engines' configuration does not change. `/mica/containers/networks` and `tmp` stay on DATA.

So that a user mount there is honoured, and an absent one fails the engine instead of filling DATA:

- **systemd.**
  - `mica-containerd.service` (today `RequiresMountsFor=/var/lib/mica /mica/containers`) adds `/mica/containers/storage`; docker's unit adds `RequiresMountsFor=/mica/containers/docker`.
  - With the user's `mica-containers-storage.mount` in the first transaction (3.2), systemd orders the engine after it and requires it. An absent disk (device timeout) or a failed mount fails the engine. Without a user mount, nothing changes.
- **OpenRC.**
  - The engines add `need mica-fstab`.
  - A Base helper, `mica-require-mount <path>`, runs in their `start_pre`: if the fstab names the engine root, it must be mounted, `nofail` or not, else the start is refused with the line named.
- micad already shows the engine as unavailable with the init's reason (`GetContainers`). Nothing else depends on the engine, so the device stays reachable.
- **Moving existing container data is the user's business.** The docs give a recipe: containers off, copy with xattrs, add the mount, containers on. Quota project 102 does not cover a moved root.

## 5. API and UI

- **Storage panel, read only:** every mount outside the image's own set (source, type, target, options, size and used), marked `user` when it comes from a user unit or fstab line.
- **Services panel:** `services.units` gains `.automount` and `.swap`.
- Nothing else.

## 6. The write map of a MicaOS root, and nosuid/nodev

From the runkit (`mica-runkit/init/boot.rs`, `mounts.rs`) and Base's units and `mica-mounts`. mica-build should assert it with a `findmnt` golden per init; `/tmp` is to be confirmed there.

| Path | Backing | Access | Options today | r3 | Quota |
| --- | --- | --- | --- | --- | --- |
| `/` | root squashfs on a signed dm-verity mapping | ro | `ro,nodev` | | - |
| `/usr`, `/etc` | overlay of the core components' verity squashfs over the root's, no upper layer | ro | - | | - |
| `/run/mica-core/<pkg>` | component squashfs on dm-verity | ro | `ro,nodev` | | - |
| `/etc/hostname` | bind of DATA `state/hostname` | rw | bind | | DATA |
| `/etc/wpa_supplicant`, `/etc/hostapd` (systemd, when installed) | binds of DATA `state/...` | rw | bind | | DATA |
| `/usr/local/lib/systemd/system` / `/usr/local/etc` | bind of DATA `state/systemd-units` / `state/openrc-local` | rw | bind | | DATA |
| `/mnt/data` | DATA, ext4 | rw | `rw,noatime[,prjquota]` | | 100/101/102 |
| `/mica` | bind of DATA `mica/` | rw | `bind,private` | `+nosuid,nodev` | 100 |
| `/mica/containers` | bind of DATA `containers/` | rw | `bind,private,nosuid,nodev` | | 102 |
| `/srv` | bind of DATA `srv/` | rw | `bind` | `bind,private,nosuid,nodev` | 100 |
| `/var` | bind of DATA `var/` | rw | `bind,private,nosuid,nodev` | | 101 |
| `/var/lib/mica` | bind of DATA `state/mica` (STATE) | rw | bind | `+nosuid,nodev` | DATA |
| `/var/lib/bluetooth` (when installed) | bind of DATA `state/bluetooth` | rw | bind | | DATA |
| `/root` | bind of `/mica/root` | rw | bind | `+nosuid,nodev` | 100 |
| `/home` | bind of `/mica/home` | rw | bind | `+nosuid,nodev` | 100 |
| `/run` | tmpfs | rw | `nosuid,nodev,mode=0755,size=32M` | | RAM |
| `/dev` | devtmpfs | rw | `nosuid,mode=0755` | | - |
| `/dev/shm` | tmpfs | rw | `nosuid,nodev,mode=1777` | | RAM |
| `/proc`, `/sys` | proc, sysfs | kernel | `nosuid,nodev,noexec` | | - |
| module/firmware composition | tmpfs | ro after composition | `nosuid,nodev,noexec` | | RAM |
| `/tmp` | to confirm per init | rw | - | | RAM |

OpenRC's `mica-mounts` makes the same binds, in the same order, with the same options.

**nosuid,nodev:**

- **`/srv` and `/home`: yes** (agreed). Nothing of the image keeps set-uid files or device nodes there. A user's binaries in `/srv` still run (no `noexec`); only set-uid bits and device nodes stop working. A compose bind source under `/srv` loses device nodes, which a container should get with `devices:` anyway.
- **`/root`: yes, safely.** It is root's home; root needs no set-uid bits, and no device nodes are kept there.
- **`/mica`: yes, after the apps removal.** Today `/mica/apps/<name>/<version>/files` holds the apps' executables; none is set-uid, since `setpriv` lives in `/usr/bin`, but nothing needs to be asserted once apps are gone. The rest of `/mica` (`config`, `ui`, `updates`, `diagnostics`, `volumes`, `home`, `root`) holds no set-uid files or devices.
  - `/mica/volumes/<project>` are containers' named-volume directories: podman already runs with `nodev` on its graphroot ("a default, not a boundary"), and volume device nodes are not a supported use.
  - `/mica/containers` already has both.
- **`/var/lib/mica`: yes** (STATE: configuration files only).
- **Defense in depth**: the runkit could mount DATA itself `nosuid,nodev`, so a bind without the options cannot reopen them. Every DATA tree can take both, as above.
  - That is a mica-deploy change (core), decided: in U2 together with the Base binds, once the QEMU suites show nothing on DATA needs either.

## 7. Split of work, tests, order

- **R (right after net-r1).**
  - core: apps removal (1.1), the application-data reset no longer clears `/srv`, openapi, UI, docs.
  - Base: `mica-data-layout` without `/mica/apps`; the `mica-accounts` app class becomes the service-account class.
  - mica-build: `reset-runtime.sh`.
- **U2 (after CG-S06).**
  - Base:
    - `srv.mount`/`mica-mounts` become `private,nosuid,nodev`; `/home`, `/root`, `/mica` and `/var/lib/mica` get `nosuid,nodev`;
    - `mica-unit` gains `.automount`/`.swap` and the mountpoint allowlist;
    - the `mica-user-mounts-generator`;
    - `mica-units` gets `After=mica-load-extensions`;
    - `mica-fstab` and `mica-unit fstab check`;
    - `mica-require-mount`;
    - `/var/mnt`.
  - mica-podman / mica-docker: the engine-root dependencies.
  - core: `.automount`/`.swap` in `services.units`; the runkit mounts DATA `nosuid,nodev` once the suites confirm it.
- **U3.**
  - core: the storage panel's mount list, and docs (the write map, a user-mount guide, the container-move recipe).
  - mica-build:
    - the write-map golden per init;
    - QEMU suites with a second virtio disk on uefi-x64 (systemd) and mini-x64 (OpenRC): a mount under `/srv`; a mount at `/srv` itself replacing the bind; a mount at the engine root with containers on it; the disk removed (engine fails with its reason, device reachable); a refused mountpoint; a swap partition; a staged reset with user mounts (disk untouched, `/srv` not cleared by the application-data tier).

## Decisions (the user, 2026-11-11)

1. r1 (managed locations) rejected; r2's app-data move dropped; the apps feature is removed with no compatibility handling (no device has apps).
2. The application-data reset stops clearing `/srv`; the system never touches user disks.
3. A user may mount at `/srv`; it replaces the default bind.
4. OpenRC: `/usr/local/etc/fstab` with `mica-fstab`.
5. `.swap` and `.automount` allowed; nothing enabled by default.
6. `/srv` and `/home` (and `/root`, `/mica`, `/var/lib/mica`) get `nosuid,nodev`; the runkit mounts DATA `nosuid,nodev` too, in U2 after the suites confirm it.
7. `mica-accounts add` stays as the service-account class.
8. Order: rustix -> offline-g8 -> net-r1 -> R -> U2/U3 after CG-S06.
