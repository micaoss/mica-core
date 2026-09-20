# 20260920-0611-remove-web-terminal The web terminal leaves the console

- **status**: completed
- **priority**: P2
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 06:11

## Description

The console shipped a "Web terminal" service card, detail page, switch and
window. None of it reached the device: `terminal-window.tsx` said so in its own
comment -- "There is no device endpoint behind it, so it shows a fixed
transcript and takes no input" -- and the switch drove a React state flag in
the simulation provider.

The user decided the feature is not being built. A root shell over HTTPS is a
new attack surface on an appliance whose management plane deliberately has no
shell, and a card that cannot do what it says is worse than no card: an
operator reads the switch as a device capability.

So it is removed rather than left as a prototype, and the only service the
catalogue keeps are the ones the device actually has a setting for.

Acceptance: no `terminal` in the console tree, `ServiceDefinition` requires
`settingsPath` and `statePath` (a card with neither could only report an
invented state), the simulation provider no longer carries the flag, and
`bash crates/mica-apid/ui/run.sh` passes.

## ActiveForm

Removing the web terminal prototype from the console

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

UI only. Removed: `terminal-window.tsx` and its test, the catalogue entry, the
detail page's terminal form and window, `terminalEnabled`/`setTerminalEnabled`,
the `services.terminal.*` and now-unused `services.detail.noObserver` keys in
both catalogues, the four `--terminal-*` colour tokens in both themes, and the
e2e block that drove the window.

- complete: removed; 234 UI tests green
