# chrono to jiff

Status: plan only. The user decides whether it goes into 0.0.9 or the round after. Until then `deny.toml` ignores RUSTSEC-2026-0543 (chrono unmaintained, every version, no fixed release).

Facts below are from mica-core main c4a9b53.

## 1. Scope

chrono is a direct dependency of three crates: `micad`, `mica-apid` and `micad-settings` (workspace `chrono = "0.4"`, features `clock`, `serde`, `std`). It is also in the lock through no other crate of ours.

- `mica-deploy`, `lifecycle-sys` and `mica-ui-bundle` do not use it. `mica-ui-bundle`'s `DateTime` is zip's, and `mica-deploy`'s certificate times are x509-parser's.
- No struct derives `Serialize`/`Deserialize` over a chrono type. Every time that reaches a wire is formatted or parsed by hand at a call site. That keeps the byte formats below in a few places that can be pinned.
- jiff replaces it: `jiff::Timestamp` for an instant, `jiff::SignedDuration` or `std::time::Duration` for spans, and `jiff::civil` plus `jiff::tz::TimeZone::UTC` for the calendar. Features: `std` only. No tz database is needed, because every time here is UTC. The version is the latest jiff at implementation, pinned in the workspace like the others.

## 2. The call sites, by kind

45 `chrono::` uses in 25 files, 82 lines that touch chrono types. By kind:

| Kind | Where | jiff | Byte format to keep |
| --- | --- | --- | --- |
| A. Now, RFC 3339, whole seconds, `Z` | micad: uplink.rs (uplink `since`, `lastChange.at`), network_state/probe.rs (`checkedAt`), update_lifecycle/policy.rs (217), update_lifecycle/snapshot.rs (`since`, `at`, `until`), system_info/json.rs (boot time from a Unix second). apid: routes/integrity.rs (256), diagnostics/collect.rs (`collectedAt`), tls.rs (certificate `notAfter`). micad-settings: audit.rs (`ts` of every audit line) | `Timestamp::now()` truncated to the second (`Timestamp::from_second(t.as_second())`), printed with `%Y-%m-%dT%H:%M:%SZ` | `2026-11-17T14:04:18Z`: chrono's `to_rfc3339_opts(Secs, true)` **truncates**; jiff's `round` would round, so it must not be used |
| B. Now, RFC 3339, milliseconds, `Z` | micad apply_queue.rs (task `createdAt`/`startedAt`/`finishedAt`); apid routes/mod.rs (an interrupted task's `finishedAt`) | `strftime("%Y-%m-%dT%H:%M:%S%.3fZ")` | `2026-11-17T14:04:18.403Z`: always three digits, truncated. jiff's `Display` trims trailing zeros (`…18.4Z`), so it must not be used |
| C. Parse RFC 3339 | apid routes/integrity.rs (staleness of a stored stamp), routes/support_info.rs (the archive name), audit.rs (test); micad update_policy.rs (tests) | `s.parse::<Timestamp>()` | Inputs are our own A/B stamps and stored integrity times. jiff accepts offsets and `Z` like chrono; a test pins `T`, `t`, `Z`, `+00:00` and a fraction |
| D. Other text formats | micad reconciler/ssdp.rs: HTTP `DATE` header `%a, %d %b %Y %H:%M:%S GMT`; apid support_info.rs: file name `%Y%m%dT%H%M%SZ` | `strftime` with the same strings | English day and month names, zero-padded day: jiff's `%a`/`%b` are English and fixed |
| E. Calendar | micad-settings configuration/window.rs (update windows: weekday Monday first, hour, minute, the last crossing of `HH:MM`, minus one day); micad update_auto.rs (anchored crossing), update_policy.rs (`install_refusal(now)`) | `Timestamp::to_zoned(TimeZone::UTC)`, `weekday().to_monday_zero_offset()`, `hour()`, `minute()`, `civil::DateTime` for the crossing, `checked_sub(1.day())` | No bytes: minutes-of-week numbers, pinned by the window tests |
| F. Arithmetic and comparison | update_lifecycle/policy.rs (`expires_at = now + seconds`, `expires_at <= now`), snapshot.rs (`waitedSeconds`), integrity.rs (older than N days), update_auto.rs (test clock `advance_wall`) | `Timestamp + SignedDuration`, `duration_since`, `<=` | `waitedSeconds` stays an integer number of whole seconds, floored at 0 |
| G. Unix seconds and microseconds | apid tls.rs (`now` as seconds for rcgen's `SystemTime`, expiry), micad provisioning_doc/import.rs (`now` as u64 seconds), apply_queue.rs (`timestamp_micros` in the boot id) | `Timestamp::now().as_second()` / `as_microsecond()`, or `SystemTime` directly where only a number is needed | The boot id stays `{micros:x}-{pid:x}` |
| H. Types in signatures and fields | update_auto.rs (`wall()`, `anchor_floor`, the test clock), update_lifecycle.rs (`Deferral.since/at`, `OverrideRecord.expires_at`), update_policy.rs, window.rs (`contains`, `last_crossing`, `minutes_of_week`) | `jiff::Timestamp` | none (internal) |

Tests that construct chrono values: micad-settings configuration/tests.rs (3), micad bus/tests/auto.rs (5), update_auto/tests.rs (2) and tests/anchors.rs (1), update_lifecycle/tests/policy.rs (3), network_state/tests/probe.rs (1), apid startup/tests/hostile.rs (1). They move with their code.

## 3. Wire formats that must stay byte-identical

- **Settings and state documents:** none holds a chrono-formatted time today. The update-window `HH:MM` and day names are strings of our own, untouched.
- **Audit ring** (micad-settings audit.rs, read by apid and `mica`): `ts` is kind A. Old and new lines must sort and parse together.
- **API** (openapi.json): every `format: date-time` member is kind A or B: task times, uplink `since`/`lastChange.at`, `checkedAt`, `collectedAt`, integrity `checkedAt`, the update snapshot's `since`/`at`/`until`, the certificate's `notAfter`. The console parses them with `Date.parse`.
- **Logs:** `MICA_UPLINK` carries no time, and tracing stamps its own lines (not chrono). No change.
- **SSDP** `DATE` header and the support archive's file name: kind D.

## 4. Tests that pin them

New before the swap, so the swap is proved against today's output:

1. A golden test per kind A, B and D, with a fixed instant: one with zero sub-seconds and one with 1 ms, 400 ms and 999 999 999 ns. It asserts the exact strings chrono produces today: `…:18Z`, `…:18.000Z`, `…:18.400Z`, `…:18.999Z` (truncated, never rounded up to `:19`).
2. A parse test (kind C) over the accepted spellings, and a refusal of a non-RFC 3339 string with the same error path.
3. The window tests (kind E) stay unchanged. They gain a case across a year end and one on a Sunday at 23:59, which are the edges of `minutes_of_week` and `last_crossing`.
4. The openapi.json schema test in micad already holds the uplink observation's keys. Value shapes for A and B are covered by test 1.

Then the swap. The same tests, unchanged, must pass on jiff.

## 5. Shape of the change

- One small module per crate that formats and parses (`time::rfc3339_secs`, `time::rfc3339_millis`, `time::parse`, `time::http_date`). Each call site above calls it instead of formatting inline, so the formats live in one place per crate. micad-settings owns them, and micad and apid already depend on it.
- The swap itself is mechanical per kind. update_auto's test clock (`advance_wall`) takes a `SignedDuration`.
- Remove chrono from the three `Cargo.toml` files and the workspace. `cargo deny` then shows chrono is gone from the lock, and the ignore in `deny.toml` is removed in the same commit.
- Commits, each gated:
  1. The golden tests and the time modules, still on chrono. This is behaviour-neutral, so the outputs are pinned first.
  2. micad-settings and apid on jiff.
  3. micad on jiff, chrono out, ignore removed.
  4. Docs: none name chrono; only `deny.toml`'s comment goes.

## 6. Effort and risk

- About a day of work plus two gates. Kinds A, B, D and G are mechanical once the module exists. Kinds E and F (update windows and anchors) need the most care, but are already covered by tests.
- Risk is in the bytes (truncation, the fraction digits) and in the calendar edges. Section 4's tests catch both before any image is built.
- No device-side data migration: no stored file holds a chrono-specific format.
- Package impact: micad, mica-apid and micad-settings change bytes. On the 0.0.9 line that is already new; on the round after, it needs the usual version bump.
- Image size: jiff with `std` only, and no tz database, is in the same range as chrono with `clock`.
