# mica-apid

`mica-apid` is the HTTPS management API and the web dashboard (crate
`crates/mica-apid`, package `mica-apid`, unit `apid.service`). The dashboard is
one client of the API it serves. apid holds no system configuration of its own:
every read and change of the device goes through micad's D-Bus interface
(`crates/mica-apid/src/bus_client.rs`).

## 1. Listeners and configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `APID_HTTPS_ADDR` | `0.0.0.0:443` | The HTTPS listener |
| `APID_HTTP_ADDR` | `0.0.0.0:80` | HTTP, redirect to HTTPS only |
| `APID_STATE_DIR` | `/var/lib/mica/apid` | TLS certificate and key, the session signing key, login-backoff counters, the audit ring |
| `APID_BUS` | `system` | The bus to reach micad on |

On first start apid generates a self-signed certificate and the cookie signing
key in its state directory and reuses them afterwards (`tls.rs`). Once both
listeners are bound it prints `APID_LISTENING https=<addr> http=<addr>` on
stdout. `mica-apid --version`, `--openapi` (print the OpenAPI document) and
`--healthcheck` (probe the running apid's `/healthz`, used by the boot health
gate) answer without starting the daemon.

## 2. URL space

| Path | Served |
| --- | --- |
| `/api/v1/...` | The JSON management API (below) |
| `/api/versions` | The API major versions this build serves |
| `/healthz` | Liveness for the boot health gate |
| `/_ui/` | The built-in single-page application, embedded in the binary |
| `/` | The active custom UI bundle if one is valid, otherwise a redirect to `/_ui/` |

Custom assets are consulted only by the final fallback, so neither UI can
shadow the API or the health endpoint.

## 3. API surface

The API is described by `crates/mica-apid/openapi.json`, generated from the
handlers (`openapi.rs`) and committed; the gate fails when the committed
document differs from `mica-apid --openapi`. Resource groups:

| Group | Routes | Backed by |
| --- | --- | --- |
| Session and setup | `GET/POST/DELETE /session`, `POST /setup`, `POST /actions/change-password` | apid (password hash in settings via micad) |
| Tokens | `GET/POST /tokens`, `DELETE /tokens/{id}` | settings `access` |
| Settings and state | `GET/PUT /settings/{path}`, `GET /state/{path}`, `GET /tasks`, `GET /tasks/{id}` | `GetSettings`, `SetSettings`, `GetState`, task records |
| Network | `GET/PUT /network`, `PUT/DELETE /network/{iface}`, WireGuard peers, `GET /network/status` | settings `network`, `GetNetworkState` |
| WiFi | `GET/POST /wifi/client/networks`, `DELETE /wifi/client/networks/{ssid}` | settings `wifi.client` |
| SSH | `GET/POST /ssh/authorized-keys`, `DELETE /ssh/authorized-keys/{fingerprint}` | settings `access.ssh` |
| Observation | `GET /time/status`, `/storage/status`, `/system/info`, `/system/telemetry`, `/health`, `/meta` | micad observers |
| Actions | `POST /actions/reboot`, `/actions/poweroff`, `/actions/transient-root-password`, `/actions/wireguard/{iface}/rotate-key` | micad actions |
| Updates | `GET /update`, `POST /update/check`, `/fetch`, `/install`, `/confirm`, `/reject`, `/rollback`, `/reboot-override`, `/config` | micad update members |
| Provisioning and claim | `GET /provisioning/status`, `GET /claim` | micad provisioning state |
| Recovery and reset | `POST /recovery/credential`, `POST /reset` | apid authority checks, micad reset |
| Diagnostics | `GET/POST /diagnostics/snapshots`, `GET/DELETE /diagnostics/snapshots/{id}` | `diagnostics.rs`, `/mica/diagnostics` |
| UI bundles | `GET /ui`, `PUT/DELETE /ui/active`, `GET/POST /ui/bundles`, `DELETE /ui/bundles/{generation}` | `bundle.rs`, `/mica/ui` |

A settings or transient-password write returns micad's task id; clients poll
`GET /tasks/{id}`. apid mirrors task records from `TaskChanged` and caches the
`access` subtree, invalidated by `SettingsChanged` (`task_registry.rs`,
`access_cache.rs`).

## 4. Authentication and authorisation

- **First run.** Until an admin password exists, `POST /setup` sets it (and
  optionally a hostname, network entries and a first API token).
- **Passwords** are hashed with argon2id and stored in the settings tree.
  Failed logins are throttled by a persistent backoff (`auth.rs`); the backoff
  applies to the password path only.
- **Browsers** exchange the password at `POST /session` for an HMAC-signed,
  HttpOnly session cookie `<id>.<mac>` backed by an in-process session table,
  valid for 24 hours (`session.rs`). Mutations from a session must carry the
  session's CSRF token.
- **Automation** uses bearer tokens `mica_<id>_<secret>`: the id is a lookup
  key, the 256-bit secret is stored only as a digest (`token.rs`). Token checks
  are not rate-limited, so a bad token cannot lock out other clients.
- **Audit.** Security-relevant actions are appended as JSON lines to an audit
  ring in the state directory and mirrored to the journal (`audit.rs`).
- **Redaction.** Values read out of the settings and state roots pass a
  structural redactor so secrets such as password hashes never leave
  (`redact.rs`).

## 5. The web UI

- **Built-in.** `crates/mica-apid/ui/` is a React + Vite application. It is
  built with Bun inside the base build image (`crates/mica-apid/ui/build.sh`)
  and embedded into the binary at compile time from `MICA_APID_UI_DIST_DIR`.
  It is always available at `/_ui/`.
- **Custom bundles.** An operator can upload a `.mica-ui.zip`
  (`POST /ui/bundles`). apid streams it to `/mica/ui` on DATA, validates and
  extracts it off the async worker (`crates/mica-ui-bundle`), and installs it as
  a new generation without activating it. `PUT /ui/active` selects a retained
  generation after repeating every safety and API-compatibility check; at
  start-up the active bundle is checked again against the API versions this
  build serves (`startup.rs`). The number of retained generations is bounded.

## 6. Diagnostics

`POST /diagnostics/snapshots` collects a bounded, redacted snapshot — system
information, telemetry, failure evidence and more — with a per-section timeout
inside an overall deadline; a section that does not answer is recorded as
absent with the reason. Snapshots are stored in `/mica/diagnostics` under a
size bound and can be exported or deleted.

## 7. The unit

`apid.service` runs as root after `micad.service`, requires the `/var/lib/mica`
and `/mica` mounts, and is confined with `ProtectSystem=strict` (writable only
`/mica/ui`, `/mica/diagnostics` and its `StateDirectory=mica/apid`),
`ProtectHome`, `PrivateTmp`, `NoNewPrivileges`, kernel and cgroup protection,
no writable-executable memory and only `AF_UNIX`, `AF_INET` and `AF_INET6`
sockets.

## 8. Compatibility with micad

mica-apid is its own executable and package so an API upgrade never repacks
micad. Today `mica-apid` depends on `micad` at exactly the same version, because
the two speak `com.mica.micad1` from one commit.
