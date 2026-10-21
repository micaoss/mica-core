# Device code and factory access-point key

A device's code names it by its hardware. It is printed on the device's label and survives
reflashing. Under a product's secret, the code also derives the device's default access-point key.
A label tool can compute that key offline from the printed code. micad implements both derivations
in `crates/micad/src/device_code.rs`.

## The consequence

The product secret ships in every image of the product, and images are public. Whoever holds a
product image's `ap-secret` can derive the default access-point key of every device of that
product from its printed device code. The default key is therefore a first-contact credential,
not a secret: it only lets the first user reach a new device.

## Where the code comes from

Each board declares one source in `/usr/share/mica/device-code.conf`, as one line
`source=<name>`. There is no fallback list: a source that appeared after a reflash would change
the code, and the printed label would no longer match.

| Source | Value read | Not a value |
| --- | --- | --- |
| `emmc-cid` | `/sys/block/mmcblkN/device/cid` of the lowest-numbered block device whose `device/type` is `MMC` (SD cards are `SD`) | absent; all `0` |
| `dt-serial` | `/sys/firmware/devicetree/base/serial-number`, without NULs | empty; all `0` |
| `dmi-serial` | `/sys/class/dmi/id/product_serial` | "To be filled by O.E.M.", "Default string", "System Serial Number", "Not Specified", "None", "0" (any case); all `0`; all `F` |
| `dmi-uuid` | `/sys/class/dmi/id/product_uuid` | `03000200-0400-0500-0006-000700080009` (AMI); all `0`; all `F` |
| `mac` | The address of the Ethernet link (`type` 1) whose `device` path sorts lowest, among links that are permanent (`addr_assign_type` 0), not wireless, not under USB, and not locally administered | no such link |

The raw value is the text read, with surrounding whitespace and NULs removed. It is not otherwise
changed. A missing file, an unknown source or no value means the device has no code.

## The code

1. `d = SHA-256("mica-device-code-v1" || 0x00 || source || 0x00 || raw value)`, where `source`
   is the name from the table.
2. The first 50 bits of `d`, read most significant bit first, become 10 characters of the
   alphabet `23456789ABCDEFGHJKLMNPQRSTUVWXYZ`, 5 bits each.
3. A Luhn mod 32 check character over those 10 characters follows. Walking from the last data
   character to the first, the factor alternates 2, 1, 2, …. Each product `factor × index` adds
   `product / 32 + product % 32` to the sum. The check character is the one at index
   `(32 − sum % 32) % 32`. It catches every single wrong character and every swap of two
   neighbours.

The canonical code has 11 characters. The printed form groups them 4-4-3: `DFN4-L34C-T8N`.
Readers drop dashes and spaces, upper-case the rest, and verify the check character.

## The SSID

The default SSID is `mica-` followed by the code's characters at indices 6 to 9: `mica-4CT8`.

## The key

1. `k = HMAC-SHA256(secret, "mica-ap-psk-v1" || 0x00 || canonical code)`.
2. Each of the first 16 bytes of `k`, taken modulo 32, is an index into the same alphabet. That
   gives 16 characters (80 bits), with no bias, since 256 is a multiple of 32.

The secret is the whole file `/usr/share/mica/ap-secret` without one trailing newline: at least 32
bytes. micad uses the file only if it is readable by its owner alone (no group or other bits).

## When the key is used

The access point's key is, in order:

1. `wifi.ap.psk`, when it is set (`pskSource: "settings"`);
2. the derived key, while the device is unclaimed, that is while it has no web administrator, and
   it has a code and a usable secret (`"derived"`);
3. the per-device key generated on STATE at first boot (`"generated"`).

An open access point uses no key, and `pskSource` is `null`. Once an administrator exists, the
derived key is never used. The claim through `POST /api/v1/setup` sets `wifi.ap.psk` in the same
write as the administrator, on a product with Wi-Fi: the key the body carries as `apPsk`, or 16
random characters of the alphabet, which the answer carries once as `apPsk` and the console shows
before it enters. A claim by provisioning document sets no key, so its device falls back to the
generated one. A configuration reset keeps the administrator and `wifi.ap.psk`. A
factory reset clears both, which makes the derived key apply again. The default SSID is the
code's whenever `wifi.ap.ssid` is unset; without a code, it comes from the device identifier.

A board that declares a source whose value is unusable, or a product secret that is missing,
short or readable by others, is logged by micad. The access point then uses the next key in the
list.

## Tools

- `micad device-code` prints this device's code and its source: `DFN4-L34C-T8N (emmc-cid)`.
  Nothing on the device prints the key or the secret.
- `micad derive-ap-key --secret FILE CODE` runs on a host. It verifies `CODE` and prints the SSID
  and key under the secret in `FILE`, as `ssid=…` and `psk=…` lines. It does not check the file's
  mode.

## Test vectors

The secret is the 32 bytes `0x00, 0x01, …, 0x1f`. The raw values are invented.

| Source | Raw value | Code | SSID | Key |
| --- | --- | --- | --- | --- |
| `emmc-cid` | `150100444a54443452a1b2c3d4e5f600` | `DFN4-L34C-T8N` | `mica-4CT8` | `U8A5TU7RT6A6E8TV` |
| `dt-serial` | `a5f2c3d4e5f60718` | `LSZJ-2L3T-WFG` | `mica-3TWF` | `PNA6B6H4FP6VTCR7` |
| `dmi-serial` | `MXQ1234567` | `Y9E3-49ET-BKD` | `mica-ETBK` | `ZYGL2RYLNSEMMJ6E` |
| `dmi-uuid` | `4c4c4544-0042-3510-8051-b4c04f4e3332` | `LVVA-BBQD-8Y4` | `mica-QD8Y` | `S8QLYFJDZT7BFJ44` |
| `mac` | `00:1a:2b:3c:4d:5e` | `RYP7-3EEM-S8X` | `mica-EMS8` | `WL2K6GAUJMRJWE6U` |

`device_code::tests::the_documented_vectors_hold` checks this table.
