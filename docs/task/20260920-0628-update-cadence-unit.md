# 20260920-0628-update-cadence-unit The update cadence is read in the unit an operator thinks in

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:28

## Description

Device testing asked for a daily update check and an automatic reboot after an
install. Most of that already exists and was only unreadable:
`updates.json` carries `checkIntervalMinutes`, `maintenance.windows` and
`rebootPolicy`, `POST /api/v1/update/config` writes all three, and the panel
offered every one of them -- the cadence as a bare minute count. "Check every
1440" is a daily check an operator has to divide to recognise.

So the field becomes a number and a unit, and the stored minute count is shown
in the largest unit that divides it exactly: 1440 reads as one day, 90 stays
ninety minutes. The document is unchanged; the unit never leaves the browser.

`rebootPolicy: window` is the automatic reboot that was asked for -- it reboots
inside the same maintenance window under the safe-to-reboot gate -- and the
selector already offers it.

What this task does **not** do: check at a chosen time of day. A cadence is an
interval from the daemon's own start, not an anchor, and micad has no field for
one. That is 20260920-0629.

Acceptance: a policy of 1440 shows `1` and `Days`, an untouched form writes
1440 back, and 6 `Hours` writes 360. `bash crates/mica-apid/ui/run.sh` passes.

## ActiveForm

Reading the update cadence in days and hours

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

UI only.

- complete: cadence unit selector; 236 UI tests green
