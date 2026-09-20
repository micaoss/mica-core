# 20260920-0640-container-management Container management on the device

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:40

## Description

Device testing asked for a container page that can add, remove, stop and edit
containers. Today the device has one `container.enabled` switch and a
reconciler that only mounts Quadlet's directory; nothing writes a unit into it,
and the console's container form is a disabled prototype.

The design is `docs/plan/20260920-0639-container-management.md`: containers are
declared in the settings tree, the reconciler renders `.container` units for
Quadlet, systemd owns their lifecycle, and podman is called **read-only** so
the console shows the engine's own answer rather than a restatement of the file
micad wrote.

## What changed

All six steps of the plan, in order.

- **Settings.** `container.units`, a map of typed entries: image, command,
  environment, published ports, volumes, restart policy, `autoStart`. Skipped
  when empty, so a device that declares none writes the document it wrote
  before containers could be declared -- `container.json` keeps schema version
  1 for that reason. `validate_container_units` refuses a name no unit could
  carry, an empty image, one host port published twice, and a volume outside
  `/mica/` or climbing out of it with `..`. The bound lives in `micad-settings`
  and not only in apid, because the document is writable without apid.
- **The reconciler** renders `50-mica-<name>.container` per entry, compares
  before writing (these files are on STATE; an unconditional rewrite is a flash
  write per reconcile), sweeps its own files and only its own, reloads so
  Quadlet regenerates, and starts each unit its entry asks to start. With the
  switch off nothing is rendered: the directory is not mounted, and a write
  would land in the image's read-only copy of it.
- **The engine is read-only.** `containers.rs` runs `podman ps --all` and
  `podman images` with a five-second bound and a 256 KiB output bound. No
  `run`, no `rm`, no `pull`. A dry-run daemon gets `NoEngine` and reports the
  engine as absent rather than as empty.
- **The bus** gains `GetContainers` -- the declared map and the engine read,
  named apart, never merged -- and `StartContainer`, `StopContainer`,
  `RestartContainer`, which drive `50-mica-<name>.service`. A name the settings
  tree does not hold is refused before any unit call, so the members are not a
  way to drive an arbitrary systemd unit through a container-shaped argument.
- **apid** serves `GET /api/v1/containers`, `PUT/DELETE
  /api/v1/containers/{name}` and `POST /api/v1/containers/{name}/{action}` for
  exactly three actions. `ContainerDeclaration` is held field-for-field against
  the settings model by a test, like `NetworkInterface`.
- **The console's** containers page lists declared and observed side by side --
  a declared container the engine never heard of and a running container nobody
  declared are both real states and the row says which -- with start, stop,
  restart, edit and a removal that says data under `/mica/` is not deleted.

## Not done, deliberately

Container **logs**. The unit's journal is reachable through the existing
failure evidence; a bounded log route can follow if it is wanted.

Also unchanged: the reset tiers. The plan flagged that `application-data`
should clear `container.units` with the applications it describes; that is
20260920-0730-reset-clears-containers and is not this task.

## ActiveForm

Bringing container management to the device

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

- complete: declared, rendered, observed and driven; all suites green
