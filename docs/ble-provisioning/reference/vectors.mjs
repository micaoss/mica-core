// Vectors for docs/ble-provisioning.md, protocol 1.
import crypto from 'node:crypto'
import { register, exchange, enc, be, hkdf, sha256 } from './spake2plus.mjs'

const hex = (b) => Buffer.from(b).toString('hex')
const b64 = (b) => Buffer.from(b).toString('base64')
const PROVER = 'mica-provisioning phone'
const VERIFIER = 'mica-provisioning device'
const KEYS_INFO = 'mica-provisioning/1 session keys'
const AAD = Buffer.from('mica-provisioning/1')

function sessionKeys(ikm, salt) {
  const k = hkdf(salt, ikm, KEYS_INFO, 64)
  return { phoneToDevice: k.subarray(0, 32), deviceToPhone: k.subarray(32) }
}
function seal(key, counter, plaintext) {
  const nonce = Buffer.alloc(12); nonce.writeBigUInt64BE(BigInt(counter), 4)
  const c = crypto.createCipheriv('aes-256-gcm', key, nonce); c.setAAD(AAD)
  return { nonce, record: Buffer.concat([c.update(plaintext), c.final(), c.getAuthTag()]) }
}
function fragments(message, size) {
  const body = size - 3; const out = []
  for (let seq = 0, off = 0; off < message.length || seq === 0; seq++, off += body) {
    const chunk = message.subarray(off, off + body)
    const last = off + body >= message.length
    const h = Buffer.alloc(3); h.writeUInt16BE(seq); h[2] = last ? 1 : 0
    out.push(Buffer.concat([h, chunk]))
  }
  return out
}

const section = (t) => console.log(`\n### ${t}\n`)
const line = (k, v) => console.log(`${k} = ${v}`)

let pwKeys
for (const [mode, pw] of [['password', 'harbour-pw-1'], ['temp-code', '40718263']]) {
  section(`SPAKE2+, auth ${mode}`)
  const salt = Buffer.from(mode === 'password' ? '000102030405060708090a0b0c0d0e0f' : 'f0e1d2c3b4a5968778695a4b3c2d1e0f', 'hex')
  const iterations = 600000
  const r = register(pw, salt, iterations, PROVER, VERIFIER)
  line('pw (UTF-8)', JSON.stringify(pw))
  line('salt', hex(salt)); line('iterations', iterations)
  line('PBKDF2 password input', hex(r.input))
  line('w0', hex(be(r.w0, 32))); line('w1', hex(be(r.w1, 32))); line('L', hex(enc(r.L)))
  const x = BigInt('0x' + sha256(Buffer.from(`x ${mode}`)).toString('hex'))
  const y = BigInt('0x' + sha256(Buffer.from(`y ${mode}`)).toString('hex'))
  line('x', hex(be(x % 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551n, 32)))
  line('y', hex(be(y % 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551n, 32)))
  const e = exchange({ context: `mica-provisioning/1 ${mode}`, idProver: PROVER, idVerifier: VERIFIER, w0: r.w0, w1: r.w1, L: r.L, x, y })
  for (const k of ['X', 'Y', 'Z', 'V', 'Kmain', 'confirmV', 'confirmP', 'Kshared']) line(k === 'X' ? 'shareP' : k === 'Y' ? 'shareV' : k, hex(e[k]))
  line('TT', hex(e.TT))
  const keys = sessionKeys(e.Kshared, Buffer.alloc(0))
  line('kPhoneToDevice', hex(keys.phoneToDevice)); line('kDeviceToPhone', hex(keys.deviceToPhone))
  console.log('\nsession messages:')
  console.log(JSON.stringify({ t: 'params', salt: b64(salt), iterations }))
  console.log(JSON.stringify({ t: 'share', shareP: b64(e.X) }))
  console.log(JSON.stringify({ t: 'share', shareV: b64(e.Y), confirmV: b64(e.confirmV) }))
  console.log(JSON.stringify({ t: 'confirm', confirmP: b64(e.confirmP) }))
  if (mode === 'password') pwKeys = keys
}

section('auth none (X25519)')
// RFC 7748 section 6.1 keys: Alice is the phone, Bob the device.
const a = Buffer.from('77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a', 'hex')
const bKey = Buffer.from('5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb', 'hex')
const priv = (d) => crypto.createPrivateKey({ key: { kty: 'OKP', crv: 'X25519', d: d.toString('base64url'), x: crypto.createPublicKey(crypto.createPrivateKey({ key: Buffer.concat([Buffer.from('302e020100300506032b656e04220420', 'hex'), d]), format: 'der', type: 'pkcs8' })).export({ format: 'jwk' }).x }, format: 'jwk' })
const pa = priv(a); const pb = priv(bKey)
const pubA = Buffer.from(crypto.createPublicKey(pa).export({ format: 'jwk' }).x, 'base64url')
const pubB = Buffer.from(crypto.createPublicKey(pb).export({ format: 'jwk' }).x, 'base64url')
const shared = crypto.diffieHellman({ privateKey: pa, publicKey: crypto.createPublicKey(pb) })
const salt = sha256(Buffer.concat([Buffer.from('mica-provisioning/1 none'), pubA, pubB]))
line('phone private', hex(a)); line('phone public', hex(pubA))
line('device private', hex(bKey)); line('device public', hex(pubB))
line('shared secret', hex(shared)); line('HKDF salt', hex(salt))
const nk = sessionKeys(shared, salt)
line('kPhoneToDevice', hex(nk.phoneToDevice)); line('kDeviceToPhone', hex(nk.deviceToPhone))
console.log(JSON.stringify({ t: 'start', auth: 'none', pub: b64(pubA) }))
console.log(JSON.stringify({ t: 'ready', pub: b64(pubB) }))

section('records (keys of the password vector)')
const k = pwKeys
const cmd = Buffer.from('{"id":1,"op":"getStatus"}')
const s1 = seal(k.phoneToDevice, 0, cmd)
line('command plaintext', cmd.toString()); line('nonce', hex(s1.nonce)); line('record', hex(s1.record))
const ev = Buffer.from('{"id":1,"event":"status","wifiMode":"ap","connected":false,"ssid":null,"ip":null}')
const s2 = seal(k.deviceToPhone, 0, ev)
line('event plaintext', ev.toString()); line('nonce', hex(s2.nonce)); line('record', hex(s2.record))
const s3 = seal(k.phoneToDevice, 1, Buffer.from('{"id":2,"op":"scanWifi"}'))
line('second command {"id":2,"op":"scanWifi"} nonce', hex(seal(k.phoneToDevice, 1, Buffer.alloc(0)).nonce)); line('record', hex(s3.record))

section('fragments of the first command record, 20-byte writes')
for (const f of fragments(s1.record, 20)) console.log(hex(f))
