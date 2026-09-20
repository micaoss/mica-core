# 20260920-0811-console-chrome-and-wifi The chrome follows micaos.dev, and Wi-Fi gains its missing halves

- **status**: completed
- **priority**: P1
- **owner**: vtv87o8e/mica-core
- **createdAt**: 2026-09-20 08:11

## Description

Four findings from a second pass over the console.

**The two pickers did not match the product.** micaos.dev renders the same two
controls as icon-triggered selects (`website/src/features/landing/components/`):
a `Globe` or the mode's own icon, the accessible name on the trigger, the
current value as the checked item. The console had a combobox with a text input
for the language -- in a 160px header slot that reads as an empty search box
rather than as the language the console is in -- and a segmented control for
the theme, buried in the settings menu. Both are selects now, both live in the
header, and the settings menu is down to what it is for.

**The console had no icon at all.** `index.html` linked no favicon, so every
tab showed the browser's blank page mark. The brand icon is now carried in the
image (`ui/public/`): the site's `favicon.svg`, `favicon-32.png` and
`apple-touch-icon.png`. The SVG is the site's file with its c2pa manifest
stripped -- 8 KB of provenance metadata about the icon, on a device image that
has no use for it and 95% of the file.

**`lo0` and `dummy` were on the interface list.** The filter hid `lo` exactly;
the kernel and some board files also spell it `lo0`, and a `dummy` link is the
kernel's placeholder, not an interface anybody configured. Both are hidden now,
and `lolink0` -- a name an integrator chose -- still is not.

**Wi-Fi could not do the three things an operator does with it.** There was no
way to configure the access point, no way to see what is on the air, and no way
to join a network that had not been typed in by hand.

## What changed

- `POST /api/v1/wifi/client/scan` and micad's `ScanWifi`: `SCAN` then
  `SCAN_RESULTS` over wpa_supplicant's control socket -- the same client the
  association read already uses -- bounded at 64 results with a two-second
  settle. POST, because a scan sweeps every channel and briefly costs the
  station its link. A hidden network is reported with an empty name rather than
  dropped; the console decides what to do with it.
- `GET/PUT /api/v1/wifi/ap`: mode, radio, SSID, key, channel, country and the
  access-point address. **`psk` reads as `<redacted>` and a write that omits it
  keeps the stored key** -- the known-network rule, for the same reason: an
  operator changing the channel was never shown the key, and treating absence
  as "no key" would publish an open access point.
- The console's Wi-Fi tab gained the access-point panel, the scan list and a
  **Connect** that opens the add dialog with the network's name filled in:
  joining a network is declaring it, which is the one write the device
  understands.

## ActiveForm

Bringing the console chrome and the Wi-Fi tab up to the product

## Dependencies

- **blocked by**: (none)
- **blocks**: (none)

## Notes

Bluetooth pairing is not here: it needs BlueZ's D-Bus surface and a pairing
agent, which is a subsystem rather than a route. The design is
`docs/plan/20260920-0812-bluetooth-pairing.md`.

Verified: `cargo test --workspace --locked` 59 of 59 suites green, workspace
clippy clean, `crates/mica-apid/ui/run.sh` green with 266 tests, OpenAPI
regenerated.

- complete: chrome, icon, filter and the wifi halves; suites green
