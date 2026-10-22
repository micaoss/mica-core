# BLE provisioning protocol, version 1

A phone sends a mica device the Wi-Fi network to join, or the addressing of its Ethernet port, over
Bluetooth Low Energy, without joining the device's access point. A device that is still unclaimed is claimed in the same session. The phone
side is a web page that uses Web Bluetooth. This document is the contract between that page and
micad. Every value below is part of protocol 1; a change to any of them is a new protocol version.

## Versioning

`info.protocol` is an integer, `1` for this document. A page supports a set of versions and stops
with "refresh the page or update the device" when the device's version is not in it. The device
speaks exactly one version. Members not listed here are not sent; a receiver ignores JSON members it
does not know.

## Windows

The device advertises the service only while a provisioning window is open. A window has a trigger
and an authentication mode:

| Trigger | Opens | Closes | Auth |
| --- | --- | --- | --- |
| `unclaimed` | every boot of an unclaimed device | 10 min after boot, or `finish` | `none` |
| `boot` | every boot of a claimed device | 10 min after boot, or `finish` | `password` |
| `offline` | a claimed device that had a default route this boot has had none for 120 s | the default route returns, or `finish` | `password` |
| `manual` | an administrator in the console, for 5, 10 or 30 min | its time, the console, or `finish` | `temp-code` |

- `boot` and `offline` open together are one window. It closes at `finish`, or once 10 minutes have
  passed since boot and a default route exists again.
- A `manual` window open at the same time as `boot` or `offline` decides the mode: `temp-code`.
- After `finish` closes an `offline` window, the device opens it again 120 s later if there is
  still no default route.
- `offline` is only for claimed devices, and only while the console setting `autoWhenOffline` is on
  (the default).
- While a window is open the adapter is powered even if Bluetooth is switched off; it returns to its
  setting when the window closes.
- A closed window stops advertising and accepts no new connection. A session already authenticated
  continues until it ends (see Sessions).
- The advertisement does not make the adapter discoverable or pairable over BR/EDR, and the service
  never asks for BLE pairing: no characteristic needs an encrypted link.

## GATT

| | UUID | Properties | Encrypted by the protocol |
| --- | --- | --- | --- |
| service `mica-provisioning` | `a7f50001-aa05-4fbe-ae8a-39652ca94ed5` | primary | |
| `info` | `a7f50002-aa05-4fbe-ae8a-39652ca94ed5` | read | no |
| `session` | `a7f50003-aa05-4fbe-ae8a-39652ca94ed5` | write, notify | no |
| `command` | `a7f50004-aa05-4fbe-ae8a-39652ca94ed5` | write | yes |
| `event` | `a7f50005-aa05-4fbe-ae8a-39652ca94ed5` | notify | yes |

- The advertisement carries the service UUID. The scan response carries the device's current
  hostname as its local name (BlueZ shortens a name that does not fit).
- Writes are write requests (with response). The page subscribes to `session` and `event`
  notifications before its first write.
- One phone at a time: while a phone is connected the device stops advertising, and a second
  central that connects anyway has every read and write refused with `org.bluez.Error.InProgress`
  (an ATT error, a rejected promise on the page), which the page reports as busy.
- While a window is open the device asks a connecting phone for a connection interval of 11.25 to
  15 ms (a phone starts at 30 to 50 ms), where its kernel lets it set the range. Each write and
  each notification waits for an interval, so the fewer and larger they are, the faster a session
  is: `maxWrite` is the largest write the ATT MTU allows, and notifications fill the MTU.

### `info`

A read returns one UTF-8 JSON object (long reads are allowed; it is not fragmented):

```json
{"protocol":1,"model":"mica cx3576","hostname":"mica-a4e2e69e","claimed":false,"auth":"none",
 "ops":["getStatus","scanWifi","claim","setWifi","setNetwork","finish"],"maxWrite":182}
```

| Member | Meaning |
| --- | --- |
| `protocol` | `1` |
| `model` | the board's model, for display |
| `hostname` | the current hostname |
| `claimed` | whether the device has an administrator |
| `auth` | `none`, `password`, `temp-code`, or `password-unavailable`: claimed, a password window, and no verifier yet (the administrator has not logged in to the console since the device gained this feature). The page says "log in to the console once to enable Bluetooth recovery" and stops |
| `ops` | the operations this device accepts. `setNetwork` is optional in protocol 1: a page offers the wired port only when it is listed |
| `maxWrite` | the largest write the page may make, header included: the connection's ATT MTU minus 3, between 20 and 512 |

`info` never holds a key, password, salt or verifier.

## Messages and fragments

Every message on `session`, `command` and `event` is split into fragments. A fragment is a 3-byte
header and up to `size - 3` bytes of the message:

| Bytes | Field |
| --- | --- |
| 0-1 | `seq`, big-endian, 0 for a message's first fragment, then +1 |
| 2 | flags: bit 0 = `last`; the other bits are 0 |
| 3.. | message bytes |

- The page writes fragments of at most `maxWrite` bytes. The device notifies fragments of at most
  the ATT MTU minus 3.
- A message is at most 16384 bytes. A fragment out of order, a `seq` over the limit, or a message
  over the limit discards the message; on `session` the device answers `bad_message`, on `command`
  it ends the session.
- A message with one fragment is header `0000 01` and the bytes.

`session` messages are UTF-8 JSON objects with a member `t`. Binary members are standard base64
with padding. `command` and `event` messages are encrypted records (see Records).

## Authentication

`session` starts and authenticates a session. The page reads `info` first and uses `info.auth`.
Any `session` message may be answered by `{"t":"error","reason":"…"}`:

| `reason` | Meaning |
| --- | --- |
| `unsupported_auth` | `auth` in `start` is not the device's current mode |
| `password_unavailable` | no verifier exists yet (see `info.auth`) |
| `locked` | too many failures; also has `retryAfter`, in seconds. The device stops advertising until then |
| `auth_failed` | the password or code is wrong; also has `attemptsLeft` before the lockout |
| `bad_message` | malformed, out of order, or not valid in this state |

A failed or refused exchange can be started again with a new `start` on the same connection.

`start` may carry `"status": true`. The device then answers `ready` with `"status": true` too, and
sends a `status` event with no `id` (see Operations) right after it, so the page needs no
`getStatus` of its own. A `ready` without the member sends none; the page asks with `getStatus`.

### `none`: X25519

Only for an unclaimed device. It protects against eavesdropping, not against a device or phone
being impersonated.

1. page -> `{"t":"start","auth":"none","pub":<32-byte X25519 public key>}`
2. device -> `{"t":"ready","pub":<32-byte X25519 public key>}`

Both sides compute `s = X25519(own private, other public)` (RFC 7748) and refuse an all-zero `s`.
Then:

```
salt = SHA-256("mica-provisioning/1 none" || phonePub || devicePub)
kPhoneToDevice || kDeviceToPhone = HKDF-SHA256(salt, s, "mica-provisioning/1 session keys", 64)
```

### `password` and `temp-code`: SPAKE2+

SPAKE2+ as in RFC 9383, ciphersuite P256-SHA256-HKDF-SHA256-HMAC-SHA256, with M and N from RFC 9383
section 4. The page is the Prover; the device is the Verifier.

| Parameter | Value |
| --- | --- |
| `idProver` | `mica-provisioning phone` |
| `idVerifier` | `mica-provisioning device` |
| `Context` | `mica-provisioning/1 password` or `mica-provisioning/1 temp-code` |
| `pw` | `password`: the administrator password as typed, UTF-8, no normalisation (the console's login uses the same bytes). `temp-code`: the 8 digits, with spaces and dashes removed |
| PBKDF | PBKDF2-HMAC-SHA256 over `len(pw) \|\| pw \|\| len(idProver) \|\| idProver \|\| len(idVerifier) \|\| idVerifier`, with the device's `salt` and `iterations`, 80 bytes out |
| `w0`, `w1` | the first and the last 40 bytes, each read big-endian and reduced modulo the P-256 group order |
| encodings | points SEC1 uncompressed (65 bytes) everywhere, the transcript's `M` and `N` included; `w0` in the transcript as 32 bytes big-endian; `len()` 8-byte little-endian |

The device stores the verifier record `{salt, iterations, w0, L = w1·P}`: a 16-byte random salt,
300000 iterations (about 2 s on an armhf board), never the password. The count travels in `params`,
so the page uses whatever the record holds; the test vectors below use 600000. The page accepts `iterations` from 10000 to 10000000 and
refuses anything else.

1. page -> `{"t":"start","auth":"password"}` (or `"temp-code"`)
2. device -> `{"t":"params","salt":<16 bytes>,"iterations":300000}`
3. page -> `{"t":"share","shareP":<X, 65 bytes>}`
4. device -> `{"t":"share","shareV":<Y, 65 bytes>,"confirmV":<32 bytes>,"attemptsLeft":n}`
5. page verifies `confirmV`; on a mismatch the password is wrong and the page stops here, telling
   the user `attemptsLeft`.
6. page -> `{"t":"confirm","confirmP":<32 bytes>}`
7. device -> `{"t":"ready"}`, or `{"t":"error","reason":"auth_failed","attemptsLeft":n}`

- Each side checks the other's share is a point on P-256 and not the identity, and aborts if not.
- `K_main`, `K_confirmP`, `K_confirmV`, `K_shared`, `confirmP = HMAC(K_confirmP, shareV)` and
  `confirmV = HMAC(K_confirmV, shareP)` are RFC 9383 section 3.4 exactly.
- `kPhoneToDevice || kDeviceToPhone = HKDF-SHA256(nil, K_shared, "mica-provisioning/1 session keys", 64)`.

**Counting failures.** The device counts the attempt as failed when it sends step 4, and persists the
count first. A valid `confirmP` takes that one failure back. So a page that gives up at step 5 has
used an attempt. `attemptsLeft` in step 4 is the attempts left after this one is counted, before
the lockout; it is optional for a receiver, which ignores members it does not know. After 5 consecutive failures the device locks for 10 minutes, then 1 hour, then 24
hours on each further lockout. Counts and lockouts survive reboots. A successful exchange resets
the consecutive count; an administrator's console login resets both. A lockout also closes a
`manual` window and voids its code. Every failure is audited.

**Verifiers.** The `password` verifier is made when the device is claimed (by `finish` or by the
console's setup), when the administrator password is changed, and at a console login when none
exists yet. Each change replaces the record, so the old password stops working. The `temp-code`
verifier is made when a `manual` window opens and is destroyed when it closes.

## Records

After `ready` both directions are AES-256-GCM:

| | |
| --- | --- |
| key | `kPhoneToDevice` for `command`, `kDeviceToPhone` for `event` |
| nonce | 4 zero bytes, then a 64-bit big-endian counter: 0 for the first record of the session in that direction, +1 per record |
| AAD | the 19 bytes `mica-provisioning/1` |
| record | ciphertext, then the 16-byte tag |

A record that does not authenticate, or a counter that would wrap, ends the session. The plaintext
of a record is one UTF-8 JSON object.

## Operations

The page writes `{"id":n,"op":"<name>",…}` to `command`; `id` is an integer the page chooses and the
device echoes. The device answers with events `{"id":n,"event":"<name>",…}`. Every operation ends
with exactly one final event; `connecting` is the only non-final one. One operation runs at a time;
another sent meanwhile gets `failed` with `busy`.

Reasons any operation can fail with: `busy`, `bad_request` (a missing or malformed member),
`unknown_op`.

Events with no `id` answer no command: `status`, when `start` asked for it, carries what
`getStatus`'s does; `closing` is described under Sessions.

| `op` | Parameters | Final event | Its own `failed` reasons |
| --- | --- | --- | --- |
| `getStatus` | — | `status`: `wifiMode` (`client`, `ap` or `off`), `connected`, `ssid` (or null), `ip` (or null), `interfaces` | — |
| `scanWifi` | — | `networks`: `networks[]` of `{ssid, signalDbm, security, channel}` | `ap_active`, `no_radio`, `scan_failed` |
| `claim` | `password`, `hostname` (optional), `apPsk` (optional) | `claimStaged` | `not_allowed`, `already_claimed`, `invalid_password`, `invalid_hostname`, `invalid_ap_psk` |
| `setWifi` | `ssid`, `psk` (`""` for an open network), `hidden` | `connected`: `ip`, `hostname` | `not_claimed`, `already_claimed`, `invalid_ssid`, `invalid_psk`, `no_radio`, `wrong_password`, `ssid_not_found`, `dhcp_timeout`, `unsupported_security`, `connect_failed` |
| `setNetwork` | `interface`, and `dhcp: true` or `static` | `networkSet`: `interface`, `carrier`, `address` (or null) | `not_claimed`, `already_claimed`, `invalid_interface`, `invalid_address`, `invalid_network`, `dhcp_timeout` |
| `finish` | — | `finished`: `claimed`, `hostname` | `already_claimed`, `not_claimed` |

`getStatus`
- `interfaces[]` lists the physical ports, by name: `{name, kind, carrier, address}`. `kind` is
  `ethernet` or `wifi`; `carrier` whether a link is up; `address` the IPv4 address with its prefix
  (`192.168.1.40/24`), or null. A page offers "wired" for an `ethernet` port and "Wi-Fi" for the
  rest of the flow.

`scanWifi`
- The device begins a scan when a phone connects and answers with networks scanned at most 30
  seconds ago, so the page's `scanWifi` rarely waits for the radio.
- `security` is `open`, `owe`, `wep`, `wpa-psk`, `wpa2-psk`, `wpa3-sae`, `wpa2-wpa3` or `wpa2-eap`,
  as the console's scan reports it. `channel` is a number; networks are not merged by name (the
  page merges them).
- While the device runs its access point it cannot scan: `ap_active`, and the page asks for the
  network name instead.

`claim` (only in a `none` session; otherwise `not_allowed`)
- `password`: 8 to 1024 bytes. `hostname`: 1-63 letters, digits or hyphens, not starting or ending
  with a hyphen. `apPsk`: the access point's key after the claim, as the console's setup takes it
  (8-63 printable ASCII characters without `"` or `\`, or 64 hex digits). A device without Wi-Fi
  refuses `apPsk` with `invalid_ap_psk`.
- Nothing is written: the claim is staged. A second `claim` replaces it. `already_claimed` when the
  device was claimed meanwhile.

`setWifi`
- `ssid`: 1-32 bytes. `psk`: `""`, 8-63 printable ASCII characters, or 64 hex digits; a `psk` is
  tried as WPA2-PSK and WPA3-SAE. `hidden`: the network does not broadcast its name.
- In a `none` session it needs a staged claim (`not_claimed`), and fails with `already_claimed` if
  the device was claimed meanwhile.
- The device saves the network first in its list, switches the radio to client mode (stopping its
  access point) and connects. It sends `{"id":n,"event":"connecting"}` once it has started, then
  `connected` or `failed`. `hostname` in `connected` is the staged one when a claim is staged.
- `connect_failed` covers what the device cannot tell apart.

`setNetwork`
- `interface`: an `ethernet` port of `getStatus`; any other name is `invalid_interface`. Exactly
  one of `dhcp: true` or `static: {address, gateway, dns}`, otherwise `bad_request`. `static` has
  the shape of the network settings' `static`: `address` an IPv4 address with its prefix
  (`10.0.0.5/24`), `gateway` an IPv4 address (optional), `dns` a list of IP addresses (optional).
  Anything else in it is `invalid_address`.
- The entry is written through the same rules as the console's network settings, replacing the
  port's addressing and keeping the rest of its entry; a refusal by those rules is
  `invalid_network`.
- In a `none` session it needs a staged claim (`not_claimed`), and fails with `already_claimed` if
  the device was claimed meanwhile, as `setWifi` does.
- `networkSet` comes once the port has its address: the `static` one, or one from a DHCP lease
  within 30 seconds (`dhcp_timeout` otherwise). Without a carrier it comes at once, with `carrier:
  false` and no address: the entry is saved and applies when a cable is plugged in.
- It leaves Wi-Fi as it is.

`finish`
- `finish` follows `setWifi`, `setNetwork`, or neither.
- In a `none` session it commits the staged claim: administrator, hostname, the `apPsk` of the
  claim if it had one, and the claim channel `ble`. `already_claimed` when another channel claimed
  first; the staged claim is dropped. `not_claimed` when nothing is staged.
- The device generates no access-point key at a BLE claim, and `finished` carries none: after
  `setWifi` the radio is a client and the access point is off. The key derived from the device
  code is the unclaimed device's only, so an access point still on without a key of its own is
  switched off at the claim; the console asks for a key when it is switched on again. The
  access-point setup (`POST /setup`) still sets a key, since its user is on the access point.
- In a `password` or `temp-code` session it only ends the session (`claimed: true`).
- After `finished` the device closes the window and disconnects.

## Sessions

- The device disconnects after 2 minutes without a `command`, not counting an operation in flight.
  The window stays open.
- Before it disconnects on its own the device may notify `{"event":"closing","reason":…}` with no
  `id`: `idle`, or `claimed_elsewhere` when another channel claimed the device during a `none`
  session.
- An unclaimed device drops a staged claim as soon as the connection drops. A reconnecting phone
  starts again from `start`.
- A claimed device's session also ends with its connection. A reconnecting phone authenticates again
  and continues with `getStatus`.

**Wi-Fi after a session.** The device notes `{enabled, mode}` before `setWifi` changes them. When the
session ends — `finish`, 2 minutes without the phone reconnecting, or the window closing — and no
`setWifi` of it ever connected, the device restores them. An unclaimed device whose session ends
without `finish` restores them even after a successful connection, and removes the networks the
session added. While a session holds the radio, the console's Wi-Fi routes answer
`423 wifi_locked`.

**Wired entries after a session.** An unclaimed device whose session ends without `finish` puts
back each entry `setNetwork` replaced. Otherwise the entries stay, as a change in the console does.

## Not covered

- The page itself, at `https://init.micaos.dev`.
- Android Chrome and Edge, desktop Chrome and Edge. iOS browsers have no Web Bluetooth; the page
  sends the user to the access-point setup.

## Test vectors

Produced by the reference in [ble-provisioning/reference](ble-provisioning/reference/README.md)
(Node.js, BigInt P-256), which first reproduces the RFC 9383 P256-SHA256-HKDF-HMAC-SHA256 vector
exactly. All values are hex unless shown as JSON. `x`
and `y` are fixed here; real sessions draw them at random. The device has counted no failures
before, so `attemptsLeft` is 4.

### SPAKE2+, auth password

```
pw (UTF-8) = "harbour-pw-1"
salt = 000102030405060708090a0b0c0d0e0f
iterations = 600000
PBKDF2 password input = 0c00000000000000686172626f75722d70772d3117000000000000006d6963612d70726f766973696f6e696e672070686f6e6518000000000000006d6963612d70726f766973696f6e696e6720646576696365
w0 = 947235c6a07ca203da652ef5f3654578849ab4e5f1ae01698be30fae331fc46f
w1 = 9977060784f931e0805507725efa9e5d1e5fd6bb8dc8e37e70ef687788d5b2fd
L = 041602a15fdc3635da663b56f662817f13c2492eef661baa61f9c6a83cab56ec78301d4c8b53a704b52a92cea7bd66b6ca0efa10879e3455112d0cd054843fedbd
x = 50ec4dec9740817c464fbd565f6a4ce13d6759645b17410600f88cb76b015cde
y = 2b5b9dd3a7a0090b90a2c46652b653f8ed7315b45d10bebdb3e88f1cbba1b28e
shareP = 04af6f17b97250531ab4e9667201b97c0781c6c7202576c5d774448f5bf52be47ff2839b992774593ec43b4ef43535e79a029e9597a3b05c23244de683ea2a085d
shareV = 04e0f08c16f158714530c1ff822a6b6c2c46a608c58ba1c616a2d42994a9cbdae2199796a5269bfc0922512c25aac4a0063a117f657b7d0dccc4fb5aaabefc3c79
Z = 04eacd194cd154026f0ee0bfa29c9f1507a16e2b64b16e8c38c2d939070994392058079d5a0812f96341387e72def06adbe618b6ff801dff32c0cd44578f55cbd1
V = 0407d56c28093d9d584d43c164b45e865da99d25d8ba5dc4dae8e95304be506180d6812c78d572b7e8c23e7658d63d857a85d5162c6abf0fc256f6cb03e1fdf11a
Kmain = 3251ce103117be1b45bf068549eb845cbc082fd374cc13fd9a791b412caf4a7f
confirmV = f13f912ed43d74d959acfda95a39fa255235fadaa444b6005a50db4f06c9e6ef
confirmP = 920c9a30aacc3a02db9a39841a9d5fcca698c5e0c8b524e2417c94cf05f8ea5c
Kshared = 96ea68a8bb6ab8b4ee86510720d8376153ef50292978fcd0d6f6c1765c187adb
TT = 1c000000000000006d6963612d70726f766973696f6e696e672f312070617373776f726417000000000000006d6963612d70726f766973696f6e696e672070686f6e6518000000000000006d6963612d70726f766973696f6e696e6720646576696365410000000000000004886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f5ff355163e43ce224e0b0e65ff02ac8e5c7be09419c785e0ca547d55a12e2d20410000000000000004d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b4907d60aa6bfade45008a636337f5168c64d9bd36034808cd564490b1e656edbe7410000000000000004af6f17b97250531ab4e9667201b97c0781c6c7202576c5d774448f5bf52be47ff2839b992774593ec43b4ef43535e79a029e9597a3b05c23244de683ea2a085d410000000000000004e0f08c16f158714530c1ff822a6b6c2c46a608c58ba1c616a2d42994a9cbdae2199796a5269bfc0922512c25aac4a0063a117f657b7d0dccc4fb5aaabefc3c79410000000000000004eacd194cd154026f0ee0bfa29c9f1507a16e2b64b16e8c38c2d939070994392058079d5a0812f96341387e72def06adbe618b6ff801dff32c0cd44578f55cbd141000000000000000407d56c28093d9d584d43c164b45e865da99d25d8ba5dc4dae8e95304be506180d6812c78d572b7e8c23e7658d63d857a85d5162c6abf0fc256f6cb03e1fdf11a2000000000000000947235c6a07ca203da652ef5f3654578849ab4e5f1ae01698be30fae331fc46f
kPhoneToDevice = bd808aa6a273ae78a8fb53434d5cf43320d2b9f3a34edb6de4afd62d243747f6
kDeviceToPhone = 794e4eceb4a026ee4ac2b6441dda6895ace89e4d7e4f34052daf56fca4e0773c

session messages:
{"t":"params","salt":"AAECAwQFBgcICQoLDA0ODw==","iterations":600000}
{"t":"share","shareP":"BK9vF7lyUFMatOlmcgG5fAeBxscgJXbF13REj1v1K+R/8oObmSd0WT7EO070NTXnmgKelZejsFwjJE3mg+oqCF0="}
{"t":"share","shareV":"BODwjBbxWHFFMMH/giprbCxGpgjFi6HGFqLUKZSpy9riGZeWpSab/AkiUSwlqsSgBjoRf2V7fQ3MxPtaqr78PHk=","confirmV":"8T+RLtQ9dNlZrP2pWjn6JVI1+tqkRLYAWlDbTwbJ5u8=","attemptsLeft":4}
{"t":"confirm","confirmP":"kgyaMKrMOgLbmjmEGp1fzKaYxeDItSTiQXyUzwX46lw="}
```

### SPAKE2+, auth temp-code

```
pw (UTF-8) = "40718263"
salt = f0e1d2c3b4a5968778695a4b3c2d1e0f
iterations = 600000
PBKDF2 password input = 0800000000000000343037313832363317000000000000006d6963612d70726f766973696f6e696e672070686f6e6518000000000000006d6963612d70726f766973696f6e696e6720646576696365
w0 = 1b91176fc9da89e1238ccc240b5a22c55fc6410f398b0f3381f75658dc2df0cf
w1 = 41e4920f9b751cd92cf8ec0825b96cfe2687b77b0db966f149c21bec7e8dfd52
L = 04b43659a2162ec5eb40cb28007f7d5c60aeda02f1d6e040c3fdd562fc111f2306d4a67f03ca8c1ceb0cd3d329f77379ee03688c034353ad5f6e295552118fe65c
x = dc299dff456bea2a25517e4eda26c6352c886a4c04ac502e847fb636e1a6a16d
y = bed8d6582ea4915d95be2bc9a8dd3d446a8ff6ec725453672c1cd024960831ad
shareP = 04f62312658aabab90930d2008392075f21ed191aadec00ee3d6eba55f6bb4c40a676db2d178f81799ed7032d608103ea0fc50076af603e196ee508e13b11722bc
shareV = 049416ad76ad99039d5a9096f8c3ce645b4ed308f40f063214f7381bb2f83d1d43ab8381df8488828feb60c71e60a04f4d3a29cc997e9909b7424fc467ac157aef
Z = 04911bc2c8c7af85dc23e55b4154d62ec33be965fba6baa41172e80c0033d4b01642dcfbf9962b7b8c88b858d5e14abab65b723ec3b1ca61d4224bdc068c89bae8
V = 04b1846a1dcaa80eef5c5851e0aa5b4923969dd76ad7070440450f52bafd3ce20dc14262d62693ac7ee0a674e5b725d51d69ff3e619f8b78ea27b547dfb54cba69
Kmain = b248f86fb4ed954d6944c67d337cc7c06145940d896a4e2ff4f878c762ede910
confirmV = 7b4db62eedf3261819e6585f8b9332d1858e4f2aff720b7d9e4208b55058f91c
confirmP = 29582fa90d5017750f2b65b0d9c0386eaf85de569a59cdbb97ba24c451ffde9d
Kshared = b767cd082d4c292f152b856efab81d1ec90d3be4e3fe5edc53695753e214d86e
TT = 1d000000000000006d6963612d70726f766973696f6e696e672f312074656d702d636f646517000000000000006d6963612d70726f766973696f6e696e672070686f6e6518000000000000006d6963612d70726f766973696f6e696e6720646576696365410000000000000004886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f5ff355163e43ce224e0b0e65ff02ac8e5c7be09419c785e0ca547d55a12e2d20410000000000000004d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b4907d60aa6bfade45008a636337f5168c64d9bd36034808cd564490b1e656edbe7410000000000000004f62312658aabab90930d2008392075f21ed191aadec00ee3d6eba55f6bb4c40a676db2d178f81799ed7032d608103ea0fc50076af603e196ee508e13b11722bc4100000000000000049416ad76ad99039d5a9096f8c3ce645b4ed308f40f063214f7381bb2f83d1d43ab8381df8488828feb60c71e60a04f4d3a29cc997e9909b7424fc467ac157aef410000000000000004911bc2c8c7af85dc23e55b4154d62ec33be965fba6baa41172e80c0033d4b01642dcfbf9962b7b8c88b858d5e14abab65b723ec3b1ca61d4224bdc068c89bae8410000000000000004b1846a1dcaa80eef5c5851e0aa5b4923969dd76ad7070440450f52bafd3ce20dc14262d62693ac7ee0a674e5b725d51d69ff3e619f8b78ea27b547dfb54cba6920000000000000001b91176fc9da89e1238ccc240b5a22c55fc6410f398b0f3381f75658dc2df0cf
kPhoneToDevice = 6306c5ec03561990cfc20c5fe35f58fc5989cc2e6f0d7c832e389b14fa4ff9c8
kDeviceToPhone = c4ebe7b4f1cc19323117793857779e4ed38319a58c46b6715b960b9cb265e5a6

session messages:
{"t":"params","salt":"8OHSw7Sllod4aVpLPC0eDw==","iterations":600000}
{"t":"share","shareP":"BPYjEmWKq6uQkw0gCDkgdfIe0ZGq3sAO49brpV9rtMQKZ22y0Xj4F5ntcDLWCBA+oPxQB2r2A+GW7lCOE7EXIrw="}
{"t":"share","shareV":"BJQWrXatmQOdWpCW+MPOZFtO0wj0DwYyFPc4G7L4PR1Dq4OB34SIgo/rYMceYKBPTTopzJl+mQm3Qk/EZ6wVeu8=","confirmV":"e022Lu3zJhgZ5lhfi5My0YWOTyr/cgt9nkIItVBY+Rw=","attemptsLeft":4}
{"t":"confirm","confirmP":"KVgvqQ1QF3UPK2Ww2cA4bq+F3laaWc27l7okxFH/3p0="}
```

### Auth none (X25519)

```
phone private = 77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a
phone public = 8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a
device private = 5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb
device public = de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f
shared secret = 4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742
HKDF salt = 78f1ec7f0df39774c9b72e0a0bb725c5d30ce7d4ed874f11dc5713d0bc62840e
kPhoneToDevice = 6adbb833f44b333ea6f2b1a438110f6f65af3176a3769bf3d0c77d04166ed39b
kDeviceToPhone = 3f0fec7695564ebbc285bd66124daf461b74a1136fc4e67eb98d086140285f8c
{"t":"start","auth":"none","pub":"hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo="}
{"t":"ready","pub":"3p7bfXt9wbTTW2HC7OQ1Nz+DQ8hbeGdNrfx+FG+IK08="}
```

### Records (keys of the password vector)

```
command plaintext = {"id":1,"op":"getStatus"}
nonce = 000000000000000000000000
record = a9b30b71223021559fb962ba91e96de295db688d93a2a506effcd08978e73840643c7973b4ed164e8d
event plaintext = {"id":1,"event":"status","wifiMode":"ap","connected":false,"ssid":null,"ip":null}
nonce = 000000000000000000000000
record = dae2e332138cc8c0b253e5c0718ce3cf6387f53731b49b2105dff0ea4e074019966babd560829e8f74ccff39310ea6fc57841635a5f3f7bd29df8f84794ca4cad0f320bdfd5e023e659406893a6fcc5386a0fcb6cbbf890254a11aef28bccb2520
second command {"id":2,"op":"scanWifi"} nonce = 000000000000000000000001
record = 2587a01eef7e85a702b0c774d68abd435dcec37814e9c80ce74a5b30023adce54fbe34bb23c755c2
```

### Fragments of the first command record, 20-byte writes

```
000000a9b30b71223021559fb962ba91e96de295
000100db688d93a2a506effcd08978e73840643c
0002017973b4ed164e8d
```
