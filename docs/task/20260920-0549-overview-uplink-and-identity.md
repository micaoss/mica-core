# 20260920-0549-overview-uplink-and-identity The overview names the uplink, and says what the device is

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 05:49

## Description

Device testing found the overview's network card useless and the page silent
about what the device is.

**The network card picks the wrong interface.** `overview-page.tsx` takes the
first observed interface that carries any address at all:

```ts
const observed = network.data?.observed.interfaces?.find((iface) => (iface.addresses ?? []).length > 0)
```

micad's observation is networkd's `Describe`, normalised but not filtered
(`crates/micad/src/network_state.rs`), and networkd describes the loopback
first. So the card reads `lo · 127.0.0.1` on a device whose uplink is up, and
says nothing about the interface an operator came to the page for.

The uplink is derivable from what the observation already carries: a route with
a `gateway` names the `interface` it leaves by. That is the definition to use --
the interface carrying a default route -- with the first non-loopback addressed
interface as the fallback when no gateway is observed, and `lo` never selected.

**The page does not say what the device is.** It already queries
`GET /api/v1/system/info` for the uptime and eight characters of the machine
id. Device identity and software version are in that same document and are what
a tester opens the page to read.

Acceptance: with a loopback and one addressed uplink observed, the card names
the uplink; with no gateway observed, it names the addressed non-loopback
interface; with only loopback, it says nothing is available rather than naming
`lo`. The page shows device identity and software version from the document it
already reads. `bash crates/mica-apid/ui/run.sh` passes.

## ActiveForm

Making the overview name the uplink and the device

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

UI only; no API, no settings, no package inputs hash moves.

- complete: uplink selection and the device panel; UI checks green
