# BLE provisioning reference

A test oracle for [the BLE provisioning protocol](../../ble-provisioning.md), not a product. It
implements the protocol's cryptography in plain Node.js — P-256 with BigInt, everything else from
`node:crypto` — slowly and without constant-time arithmetic. Use it to check an implementation's
values, never to provision a device.

| File | What it does |
| --- | --- |
| `spake2plus.mjs` | SPAKE2+ (RFC 9383, P256-SHA256-HKDF-SHA256-HMAC-SHA256): registration, both shares, Z, V, the transcript and the key schedule |
| `rfc-check.mjs` | reproduces the RFC 9383 Appendix C P-256 vector; exits non-zero on any mismatch |
| `vectors.mjs` | prints the protocol's test vectors, as listed in the document |

```sh
node rfc-check.mjs
node vectors.mjs
```

Node.js 20 or newer; no packages.
