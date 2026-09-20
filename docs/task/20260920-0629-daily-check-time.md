# 20260920-0629-daily-check-time A check at a chosen time of day

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:29

## Description

Device testing asked to "check once a day at a given time". The device cannot
do that today and the console must not pretend it can.

`update_auto.rs` schedules from `checkIntervalMinutes`, an interval measured
from the daemon's own start. Two devices booted an hour apart with the same
daily cadence check an hour apart, and a reboot moves the anchor. The
maintenance windows are not the answer either: they gate install and reboot,
not the metadata check.

What it would take: an anchor in the operator's update document -- a time of
day and the timezone it is read in -- carried through `UpdateConfigPatch`, the
resolved policy and the automatic scheduler, with the existing interval as the
fallback when no anchor is set. The presentation timezone already exists as
`time.timezone`, so the device has a reading of "03:00 local" that is not the
browser's.

Not started; raised so the gap is recorded rather than approximated in the UI.

## Annotations

- 2026-09-20: implemented as `checkAt` in **UTC**, not in a carried timezone.
  The description above proposed carrying the presentation timezone with the
  anchor; `MaintenanceWindow` already answers that question the other way, and
  for the reason it gives -- the device clock is UTC and `time.timezone` is
  presentation only -- so a second convention beside it would be a second
  reading of the same hour. The console labels the field `(UTC)`, as it labels
  the windows.
- 2026-09-20: an anchor crossed while the device was down does not fire at
  boot (the floor is seeded with the driver's start). A device that is off at
  its anchor therefore waits for the next crossing; the interval remains the
  answer for a device that is not on at a predictable hour. The alternative --
  firing at boot for a missed anchor -- makes a device in a reboot loop check
  every boot, which is exactly what the interval's own start-seeded floor
  exists to prevent.

## ActiveForm

Anchoring the automatic update check to a time of day

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

- complete: checkAt in UTC through the document, the resolution, the driver, the route and the console
