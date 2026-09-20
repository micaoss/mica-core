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
| WiFi | `GET/PUT /wifi/client`, `GET/POST /wifi/client/networks`, `PUT/DELETE /wifi/client/networks/{ssid}`, `POST /wifi/client/scan`, `GET/PUT /wifi/ap` | settings `wifi`; the scan is POST because it puts the radio to work, and `psk` reads redacted and is kept when a write omits it |
| Containers | `GET /containers`, `PUT/DELETE /containers/{name}`, `POST /containers/{name}/start|stop|restart` | settings `container.units` and micad's engine read; the three verbs are POST-only |
| Bluetooth | `GET/PUT /bluetooth`, `POST /bluetooth/discovery`, `POST /bluetooth/devices/{address}/{pair,confirm}`, `DELETE /bluetooth/devices/{address}` | settings `bluetooth` and micad's BlueZ read; the adapter write keeps the trust list, which only pairing changes |
| MQTT | `GET/PUT /mqtt` | settings `mqtt` and the reconciler's live state; the listener is written as one document because an address and a port take effect together |
| SSH | `GET/POST /ssh/authorized-keys`, `DELETE /ssh/authorized-keys/{fingerprint}` | settings `access.ssh` |
| Observation | `GET /time/status`, `/storage/status`, `/system/info`, `/system/telemetry`, `/health`, `/meta` | micad observers |
| Actions | `POST /actions/reboot`, `/actions/poweroff`, `/actions/transient-root-password`, `/actions/wireguard/{iface}/rotate-key` | micad actions |
| Updates | `GET /update`, `POST /update/check`, `/fetch`, `/import`, `/install`, `/confirm`, `/reject`, `/rollback`, `/reboot-override`, `/config` | micad update members; `/import` streams an uploaded `.micaupd` to the device first |
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
  optionally a hostname and network entries). It mints no API token: the
  password it creates is the device's one credential, and a client that wants
  a bearer token asks for one at `POST /tokens`.
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

### Reaching a device shell

Written here rather than only in the source, because it is what a person at a
bench needs.

1. **Claim the device.** `POST /api/v1/setup` with a password of at least 8
   bytes, on an unclaimed device. It answers **201** with an API token that is
   not recoverable afterwards. A device claimed by a provisioning document
   (`mica-provisioning.toml`, `[admin]`) is already past this step.
2. **Open SSH.** `access.ssh.enabled` is **false** on a fresh device, whatever
   the image; the operator turns it on. Persistent access is a public key in
   `access.ssh.authorizedKeys`, rendered for **both** managed accounts, `root`
   and `mica`.
3. **Or set a transient root password**, for the case a key cannot cover -- an
   operator in front of a device with no key installed yet:
   - API: `POST /api/v1/actions/transient-root-password`, body
     `{"password": "..."}`, 8 to 72 bytes, no NUL, newline or carriage return.
     Answers **202**; a **422** names the bound it broke and never repeats the
     password.
   - UI: the **Access** page, *Transient root password*, hinted "8-72 bytes;
     removed at the next reboot."
   - It is written into no setting, never logged, and gone at the next boot
     (`crates/micad/src/transient.rs`). It sets the `root` hash in the
     STATE-backed `/etc/shadow`, with its marker beside that file.

**What authenticates the password.** `dropbear` on this image authenticates
against `/etc/shadow` through `crypt(3)`; it does **not** use PAM. Measured
2026-09-20 from the pinned `dropbear-bin 2025.89-1~deb13u1`
(`mica-system-base:locks/upstream.lock`): its `Depends` name no `libpam`, and
`/usr/sbin/dropbear` links `libtomcrypt`, `libtommath`, `libz`, `libcrypt`,
`libc` and the loader -- no `libpam.so.0`. **That is a property of the Debian
package, not a choice recorded anywhere**, and micad's transient password
depends on it: a dropbear that grew a PAM dependency would make this path stop
working with no other symptom. The assertion belongs where the package is
pinned and the root composed (`mica-system-base`), not here
(`docs/task/20260920-0620-ssh-without-pam.md`).

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
