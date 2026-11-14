# `mica`: one operator command

Status: approved plan (cli-20261114), with the master's corrections: `diag` becomes `mica doctor`; a wrong current password counts in the login backoff; `mica set` types its value by the setting.

The user's decisions are built in:

- `mica` is the only operator entry on the device.
- The `mica-update` and `mica-ctr` links go. Their verbs move to `mica update` and `mica container`.
- The daemons are reached only as their units start them. Device code, AP key derivation and board check move under an unlisted `mica internal`.
- `mica unit` is the one unit command. Base's `mica-unit` leaves PATH if micad still needs it as a backend.
- Every subcommand and verb is a full word: `container`, not `ctr`; `doctor`, not `diag`; `list`, not `ps`. No short aliases are kept.

Facts below are from mica-core main 953e4fc, with mica-system-base and mica-build origin/main read only.

## 1. Where things are today

`/usr/bin/micad` is one executable. `pkgs/micad/Dockerfile` links `mica-apid`, `mica-update` and `mica-ctr` to it, and `crates/micad/src/main.rs` dispatches on argv0, then on the first word:

| Today | Module | Transport |
| --- | --- | --- |
| `micad daemon` | `micad::main` | owns `com.mica.micad1` |
| `micad apid`, `mica-apid` | `mica_apid::main` | serves HTTPS and `/run/mica/apid.sock` |
| `micad update`, `mica-update` | `update_cli.rs` | bus |
| `micad ctr`, `mica-ctr` | `ctr_cli.rs` (`up`, `down`, `list`/`ps`, `check`, `start`, `stop`, `restart`, `logs`) | bus |
| `micad unit` | `unit_cli.rs` (`install`, `enable`, `disable`, `remove`, `start`, `stop`, `restart`, `list`, `logs`) | bus; micad calls Base's `/usr/sbin/mica-unit` (`user_units.rs` `PROGRAM`) |
| `micad web` | `web_cli.rs` (`status`, `network`, `local`, `off`) | bus |
| `micad mdns` | `mdns_cli.rs` (`status`, `list`, `on`, `off`, `add`, `remove`) | bus, plus `/run/mica/mdns.d` |
| `micad ssdp` | `ssdp_cli.rs` (`status`, `on`, `off`) | bus |
| `micad factory-reset` | `factory_reset_cli.rs` | bus `FactoryReset`, which checks the password with micad's own backoff (`admin_password.rs`) |
| `micad diag` | `mica_apid::diag_main` | snapshot |
| `micad device-code`, `micad derive-ap-key`, `micad board check` | `device_code.rs`, `board.rs` | local files |

The credential rules live in apid, not on the bus:

| Rule | Where |
| --- | --- |
| argon2id hash and verify | `mica-apid/src/auth.rs` |
| Login backoff, persisted under `APID_STATE_DIR` | `auth.rs` `GuardStore` |
| Audit ring for operator actions | `mica-apid/src/audit.rs` (shared `micad_settings::append_audit_line`) |
| Setup, with the device-code gate and the claim lock | `routes/setup.rs` |
| Password change and the bootstrap-claim discharge | `routes/password.rs` |
| Transient root password | `routes/password.rs`, then bus `SetTransientRootPassword` |
| API tokens | `token.rs`, `routes/tokens.rs` |
| SSH authorized keys | `routes/ssh.rs` |
| Settings validation beyond micad's model | `routes/validate*`, `routes/settings.rs` |

apid already serves its whole API on `/run/mica/apid.sock` for a command line on the device (`routes/local.rs`). The socket is `0600`, the caller's uid comes from `SO_PEERCRED`, only root passes, and the credential is `LocalCredential` (no CSRF). The audit `Source` of such a caller is `local`. apid serves this socket in `network` and `local` web modes. In `off` apid does not run.

## 2. The rule: one route per rule owner

A verb goes where its rules live, so `mica` adds no rule and copies none:

- **apid's local socket** for every verb whose checks are apid's: setup, password, root password, tokens, SSH keys, `get`/`set`, hostname, time, network, Wi-Fi. The CLI then gets the API's validation, audit lines (source `local`) and login backoff because it *is* an API call. Nothing is missing on the bus for these.
- **The bus** for verbs whose rules are micad's: `update`, `container`, `unit`, `web`, `mdns`, `ssdp`, `factory-reset`, `reboot`, `poweroff`, `status`. These are today's CLIs, renamed. They keep working with apid off, which `mica web local|network` needs to turn it back on.
- **Local files** for `internal`, as today.

Rejected: moving setup, password, tokens and SSH keys onto the bus. That would mean a second hash/verify path and a second backoff counter (apid's persisted `GuardStore` versus micad's in-memory `PasswordBackoff`), plus a copy of apid's validators in micad, all for a command line that apid already serves. It would also need new bus methods (`SetupAdmin`, `ChangeAdminPassword`, `MintToken`/`RevokeToken`, `AddSshKey`/`RemoveSshKey`) and a privileged caller check on each.

With apid off (`access.web.mode` `off`), an apid-routed verb exits 3 with `apid is off (access.web.mode off); mica web local serves the local socket only`. Nothing falls back silently.

### Missing on the bus

Nothing is required. `status` needs no new method: it composes `GetSystemInfo`, `GetUpdateState`, `GetNetworkState`, `GetTimeStatus` and `GetStorageStatus`.

## 3. Verbs

`mica help` prints one line per verb. `mica help <verb>` and `mica <verb> help` print that verb's usage. `--json` is accepted by every reading verb. `--version` is kept as a flag, as `wants_version` accepts it today.

Passwords, PSKs and the device code are never taken from argv. They come from a prompt with echo off (asked twice for a new password, as in `factory_reset_cli.rs`), or from `--password-file FILE` (`-` is stdin), whose first line is the value. This reuses the bounded reader already in `factory_reset_cli.rs`.

| Verb | Does | Route |
| --- | --- | --- |
| `mica status` | version, deployment and update state, network addresses, time sync, DATA use | bus: `GetSystemInfo`, `GetUpdateState`, `GetNetworkState`, `GetTimeStatus`, `GetStorageStatus` |
| `mica setup [--hostname NAME] [--password-file FILE] [--device-code-file FILE]` | first-run setup. Asks for the device code when the device has one (`GET /v1/session` says so) | `POST /v1/setup` |
| `mica password change [--current-password-file FILE] [--password-file FILE]` | change the admin password | `POST /v1/actions/change-password` |
| `mica root-password [--password-file FILE]` | the transient root password | `POST /v1/actions/transient-root-password`, then waits for the task |
| `mica ssh status` / `on` / `off` | whether sshd runs; `access.ssh.enabled` | `GET`/`PUT /v1/settings/access/ssh/enabled` |
| `mica ssh keys list` / `add <file\|->` / `remove <fingerprint>` | authorized keys | `/v1/ssh/authorized-keys[/{fingerprint}]` |
| `mica tokens list` / `create <name>` / `revoke <id>` | API tokens. `create` prints the secret once | `/v1/tokens[/{id}]` |
| `mica get <path>` | one setting, dot path | `GET /v1/settings/{path}` |
| `mica set <path> <value>` | one scalar setting. The value is typed by the setting: `mica` reads the current value first; a string setting takes the argument verbatim (`mica set hostname 1234` is the string `"1234"`), a boolean takes `true` or `false` only, a number a number, an array or object JSON. An unknown or absent path is refused with the path named. apid decides what it accepts (`hostname`, `access.*.enabled`, `container.enabled`, `mqtt.enabled`, `wifi.*`, `time.*`, and so on) and refuses the rest with the path named | `PUT /v1/settings/{path}` |
| `mica hostname [NAME]` | show or set | `/v1/settings/hostname` |
| `mica time` / `mica time zone [ZONE]` / `mica time zones` / `mica time servers [SERVER...]` | time status, zone, NTP servers (the API writes no NTP switch, only `time.ntp.servers`) | `/v1/time/status`, `/v1/time/zones`, settings `time.timezone`, `time.ntp.servers` |
| `mica network` / `mica network show <iface>` | interface state and addresses | `/v1/network/status`, `/v1/network/{iface}` |
| `mica wifi` / `mica wifi scan` / `mica wifi on\|off` / `mica wifi add <ssid> [--password-file FILE] [--hidden]` / `mica wifi remove <ssid>` | client status, scan, saved networks | `/v1/wifi/client`, `/v1/wifi/client/scan`, `/v1/wifi/client/networks[/{ssid}]` |
| `mica update [status]` / `check` / `install [--reboot]` / `rollback [--reboot]` | as `mica-update` | bus, `update_cli.rs` |
| `mica container list` / `up` / `down` / `check` / `start` / `stop` / `restart` / `logs` | as `mica-ctr`, with `ps` dropped | bus, `ctr_cli.rs` |
| `mica container status` / `on` / `off` | `container.enabled` | bus `GetSettings`/`SetSettings` (micad's rule) |
| `mica unit install` / `enable` / `disable` / `remove` / `start` / `stop` / `restart` / `list` / `logs` | as `micad unit` | bus, `unit_cli.rs` |
| `mica web status` / `network` / `local` / `off` | as `micad web` | bus |
| `mica mdns ...`, `mica ssdp ...` | as today | bus |
| `mica factory-reset [--password-file FILE]` | as today, micad's backoff | bus `FactoryReset` |
| `mica reboot`, `mica poweroff` | | bus `Reboot`, `PowerOff`, as `update --reboot` uses today |
| `mica doctor` | the diagnostic snapshot, as `micad diag` | unchanged |
| `mica help [verb]` | | |
| `mica internal device-code` / `derive-ap-key --secret FILE CODE` / `board check FILE` | build and factory tools, not listed in `mica help` | unchanged |

`mica reboot` and `mica poweroff` use the bus, not apid's routes, so that they work with apid off. Neither surface writes an audit line for them today (`routes/power.rs` passes through to the same bus methods), so the rules stay the same.

Not in scope: owner certificates, extensions, WireGuard peers, DNS records, Bluetooth, the AP, MQTT and UI bundles. `get`/`set` and the console reach them. Each can become a verb later if asked.

## 4. Output, `--json`, exit codes

- **Human output by default.** Short `key: value` lines and aligned tables, as `web status` and `ssdp status` print today. Errors go to stderr as `mica: <message>`, using the API's or the bus error's message. `mica set` names the refused path.
- **`--json`.** On success, the answer's JSON document goes to stdout as the API or bus returned it, with no rewrapping. On failure, stdout is empty and stderr carries one JSON object `{"error": {"code": ..., "message": ..., "path": ...}}`. For an apid route this is the `ApiError` code (`validation_failed`, `login_throttled`, ...). For the bus it is the D-Bus error name's last part. `status --json` is one object with a member per bus call.
- **Exit codes**, stable and documented:

| Code | Meaning |
| --- | --- |
| 0 | done |
| 1 | refused or failed: validation, conflict, a failed task, any other error |
| 2 | usage: unknown verb, missing or extra argument |
| 3 | unreachable: micad not on the bus, apid's socket absent, apid off, or a bus/API timeout |
| 4 | credential: wrong password or device code (401/403), or throttled (429 `login_throttled`, micad `login_throttled`) |

Tasks (202 with a task id) are waited for by default, as `unit` and `ctr` do today, with the task's own result deciding 0 or 1.

## 5. Implementation

- **Entry.** `crates/micad/src/main.rs`: argv0 `mica` goes to a new `mica_cli::main`, which dispatches the verbs above to the existing modules (renamed verbs only) and to new ones. `micad <verb>` keeps working only for `daemon`, `apid` and `--version`. Every other `micad` word exits 2 with "use mica <verb>", since there are no devices and no compatibility to keep.
- **Local-socket client.** A small HTTP/1.1 client over `UnixStream` to `APID_LOCAL_SOCKET_PATH` (`hyper` client on tokio, already in the tree through axum/apid), with method, path, JSON body, status and JSON answer, and a 202 task wait through `GET /v1/tasks/{id}`. It is shared with `healthcheck.rs` only if that stays simpler.
- **New modules** in `crates/micad/src/cli/`: `mod.rs` (dispatch, help, exit codes, `--json`, secret input), `api.rs` (socket client), `account.rs` (setup, password, root-password, tokens, ssh), `settings.rs` (get, set, hostname, time), `network.rs` (network, wifi), `status.rs`, `power.rs`. The existing `*_cli.rs` move under `cli/` with their verbs renamed.
- **Packaging.** `pkgs/micad/Dockerfile`: add `/usr/bin/mica -> micad`; remove `mica-update` and `mica-ctr`. `mica-apid` stays a link: `apid.service`/`apid.openrc` start it, and Base's `mica-health` runs `mica-apid --healthcheck`. It is not an operator command and `mica help` does not list it.
- **Version.** 0.0.8 line, already bumped. No release until the next round.

## 6. Tests

- **Parse tables** per verb, as the existing `every_command_parses_and_a_misuse_is_named`: every listed verb parses, every abbreviation and other spelling (`ctr`, `ps`, `diag`, `diagnostics`) and every old `micad` word exits 2, and `help` lists exactly the verbs in the table (`internal` absent).
- **Secrets**: no verb accepts a password or PSK in argv; `--password-file -` reads stdin; a password file over the bound is refused.
- **Exit-code mapping**: unit tests from `ApiError` codes, HTTP statuses and bus error names to 0–4.
- **`mica set` typing**: a string setting takes `1234` as a string, a boolean refuses anything but `true`/`false`, an absent path is refused with its name.
- **Change-password backoff**: wrong current passwords reach `login_throttled` (429, exit 4).
- **Local-socket e2e** (`crates/mica-apid/tests/e2e`, real micad + apid on a private bus, as `web_flow` does): `mica setup` with and without a device code; `mica password change` with the wrong current password (exit 4, audit line `password wrong-password` source `local`); login backoff reached through `mica setup` with wrong device codes (exit 4, `login_throttled`); `tokens create/list/revoke`; `ssh keys add/list/remove`; `set` of an accepted scalar and a refused path; `--json` shapes; apid stopped gives exit 3.
- **Bus verbs**: the existing `update`, `ctr`, `unit`, `web`, `mdns`, `ssdp`, `factory-reset` tests are renamed; `status` against the test bus prints each member and `--json` is one object.
- **`executable.rs`**: `mica` via argv0, the removed links, and bare `micad` still exiting 2.

## 7. Docs

- `docs/mica-core.md`: one operator-command section (the verb table, routes, `--json`, exit codes, secret input), replacing the scattered `micad ctr`, `micad update`, `micad unit`, `micad web`, `micad mdns`, `micad ssdp`, `micad factory-reset`, `micad diag` and `micad device-code` mentions (21 lines). `docs/factory-ap-key.md` gets `mica internal device-code` and `mica internal derive-ap-key`.
- The console's own hints (`ui/src/features/access/api-gone.tsx`, `web-panel.tsx`) change `micad web network` to `mica web network`, with i18n where the text is a resource.
- `micad` and `mica` top-of-file docs in `main.rs` and in the Dockerfile comment.

## 8. Base and build impact (requests, not edits)

- **mica-system-base.** Move `/usr/sbin/mica-unit` to `/usr/lib/mica/mica-unit` in mica-systemd and mica-openrc, along with its tests (`tests/payload/unit.test.ts`, `mounts.test.ts`) and `docs/floor-and-options.md` ("`micad unit` on the device" becomes `mica unit`). micad still calls it as the backend of `mica unit` (`user_units.rs` `PROGRAM`). Core changes `PROGRAM` in the same round. There is no compatibility path: the Base release and the core release are pinned together in one mica-build round. `mica-health` keeps `mica-apid --healthcheck`.
- **mica-build.**
  - `src/verify/checks-file-root.ts` and its tests expect `/usr/bin/mica-update` to be a link to micad. That becomes `/usr/bin/mica`; `mica-apid` stays.
  - `src/rootfs/runtime/consumers.json` names `/usr/sbin/mica-unit`, which becomes `/usr/lib/mica/mica-unit`.
  - Suites move to the new verbs:
    - `tests/suites/lifecycle-uefi/containers.sh` (4 calls)
    - `gce.sh` (2)
    - `apid-api/run.sh` (3)
    - `apid-api/src/config.ts` (1)
    - `tests/gates/board-interfaces.test.ts` (`micad board check` becomes `mica internal board check`)
  - `boards/products.md` and the cx3576 control text change `micad device-code` and the board-default wording.
  - All of this lands with the pin move that brings this core.
- **mica-pkgs, mica-podman, mica-docker.** Nothing found.

## 9. Rounds and effort

1. **Entry and renames** (about 1 day): `mica` link and dispatcher, help, exit codes, `--json` on the existing verbs, and the renames (`container`, `doctor`, `internal`, `mica unit`). Old links and `micad <verb>` go. `status`, `reboot`, `poweroff`, and `container status|on|off` on the bus. Docs for these. Lands on main with the gate. Build is told its suites' new verbs before the next pin move.
2. **Local-socket verbs** (about 1.5 days): socket client, `setup`, `password change`, `root-password`, `ssh`, `tokens`, `get`/`set`, `hostname`, `time`, `network`, `wifi`, the e2e cases and the docs.
3. **Base hand-off** (core side under an hour once Base has released): `PROGRAM` moves to `/usr/lib/mica/mica-unit` when Base's release with the move exists, in the same product round.

The plan doc itself lands as round 1's first commit.

## 10. Decisions on the review's questions

1. A wrong current password on `POST /v1/actions/change-password` counts in the login backoff (`GuardStore`), on the API and so on `mica` (round 2, with a test).
2. `mica set` types its value by the setting (section 3), not by whether it parses as JSON.
