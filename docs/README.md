# mica-core documentation

| Document | Read it for |
| --- | --- |
| [architecture.md](architecture.md) | What mica-core is, its components, how they interact on a device, security boundaries, the source map |
| [features.md](features.md) | What a device can do, and which crate and page own each capability |
| [development.md](development.md) | Building, testing, running the daemons locally, and how to make common changes |
| [design/micad.md](design/micad.md) | The management daemon: settings, reconcilers, the D-Bus interface, provisioning, updates, reset |
| [design/apid.md](design/apid.md) | The HTTPS API and web UI: routes, authentication, UI bundles, diagnostics |
| [design/mqtt.md](design/mqtt.md) | The MQTT broker and the application-data bridge |
| [design/deployment.md](design/deployment.md) | Signed deployments, early boot and shutdown: mica-deploy, mica-runkit, lifecycle-sys |
| [design/packaging-and-release.md](design/packaging-and-release.md) | Build images, producers, gates, CI and releases |
| [../pkgs/README.md](../pkgs/README.md) | The packaging contract every producer follows |

The HTTP API is specified by [`crates/mica-apid/openapi.json`](../crates/mica-apid/openapi.json).

Project records: [`task/`](task/index.md) (work items), [`plan/`](plan/index.md)
(designs and proposals, including ones not implemented) and
[`changelog.md`](changelog.md).

System-wide design shared by every Mica OS repository — storage layout,
release signing, the access model, boards — lives in
the `mica` repository of the `micaoss` organisation, under `docs/`.
