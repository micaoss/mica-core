# 20260920-1010-system-log-view A read-only system log in the console

- **status**: pending
- **priority**: P3
- **owner**: (unassigned)
- **createdAt**: 2026-09-20 10:10

## Description

Device testing asked whether the console should show a system log. It has no
log view today. What exists is adjacent and not the same thing:

- `POST /api/v1/diagnostics/snapshots` collects a bounded, redacted snapshot --
  system information, telemetry and failure evidence -- stored under
  `/mica/diagnostics` and exportable. That is evidence for somebody else to
  read later, not a window an operator watches now.
- The audit ring records what the management plane was asked to do
  (`update-check`, `update-config`, logins), not what the system did.

What a log view needs decided before it is built:

- **Read-only page or a live follow.** A bounded read (`journalctl` with a
  unit filter, a line bound and a since/until) is a route like any other. A
  follow is a long-lived connection through apid, which nothing here has:
  every route today answers and closes, and a streaming surface brings
  back-pressure, per-session bounds and a second authentication path for an
  upgrade request. The read is the smaller half and answers most of the ask.
- **What may leave the device.** The journal carries whatever a unit printed,
  which is not redacted the way a snapshot's sections are. A log route needs a
  unit allowlist (micad, apid, the broker, the reconciled units) rather than
  the whole journal, or it becomes a way to read anything any process logged.
- **Where the bound is.** Lines, bytes and a deadline, the way every other
  bounded read here is written.

Recommendation: the bounded read first (`GET /api/v1/logs?unit=&lines=&since=`
over an allowlist), and a follow only if watching a unit live turns out to be
what the device test meant.

## ActiveForm

Deciding and building the system log view

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)
