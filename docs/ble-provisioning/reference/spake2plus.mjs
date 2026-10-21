// Reference implementation of the mica BLE provisioning crypto, protocol 1.
// A test oracle: it checks RFC 9383 and produces the vectors in docs/ble-provisioning.md.
import crypto from 'node:crypto'

const p = 0xffffffff00000001000000000000000000000000ffffffffffffffffffffffffn
const n = 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551n
const a = p - 3n
const b = 0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604bn
const G = [0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296n, 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5n]

const mod = (x, m = p) => ((x % m) + m) % m
const inv = (x, m = p) => { let [r0, r1, s0, s1] = [mod(x, m), m, 1n, 0n]; while (r1) { const q = r0 / r1;[r0, r1] = [r1, r0 - q * r1];[s0, s1] = [s1, s0 - q * s1] } return mod(s0, m) }
function add(P, Q) {
  if (P === null) return Q
  if (Q === null) return P
  if (P[0] === Q[0]) {
    if (mod(P[1] + Q[1]) === 0n) return null
    const l = mod((3n * P[0] * P[0] + a) * inv(2n * P[1]))
    const x = mod(l * l - 2n * P[0]); return [x, mod(l * (P[0] - x) - P[1])]
  }
  const l = mod((Q[1] - P[1]) * inv(Q[0] - P[0]))
  const x = mod(l * l - P[0] - Q[0]); return [x, mod(l * (P[0] - x) - P[1])]
}
export function mul(k, P) { let R = null; let A = P; k = mod(k, n); while (k) { if (k & 1n) R = add(R, A); A = add(A, A); k >>= 1n } return R }
const neg = (P) => [P[0], mod(-P[1])]
const big = (buf) => BigInt('0x' + (Buffer.from(buf).toString('hex') || '0'))
const be = (x, len) => Buffer.from(x.toString(16).padStart(len * 2, '0'), 'hex')
export const enc = (P) => Buffer.concat([Buffer.from([4]), be(P[0], 32), be(P[1], 32)])
export function dec(buf) {
  if (buf.length !== 65 || buf[0] !== 4) throw new Error('not an uncompressed point')
  const P = [big(buf.subarray(1, 33)), big(buf.subarray(33))]
  if (P[0] >= p || P[1] >= p || mod(P[1] * P[1] - (P[0] ** 3n + a * P[0] + b)) !== 0n) throw new Error('not on P-256')
  return P
}
function decompress(hex) {
  const buf = Buffer.from(hex, 'hex'); const x = big(buf.subarray(1))
  let y = modpow(mod(x ** 3n + a * x + b), (p + 1n) / 4n)
  if ((y & 1n) !== BigInt(buf[0] & 1)) y = p - y
  return [x, y]
}
function modpow(x, e) { let r = 1n; x = mod(x); while (e) { if (e & 1n) r = mod(r * x); x = mod(x * x); e >>= 1n } return r }
export const M = decompress('02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f')
export const N = decompress('03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49')

const len8 = (buf) => { const l = Buffer.alloc(8); l.writeBigUInt64LE(BigInt(buf.length)); return Buffer.concat([l, buf]) }
const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest()
const hmac = (key, msg) => crypto.createHmac('sha256', key).update(msg).digest()
export const hkdf = (salt, ikm, info, length) => Buffer.from(crypto.hkdfSync('sha256', ikm, salt, Buffer.from(info), length))

/// w0, w1 and L from a password (mica profile).
export function register(pw, salt, iterations, idProver, idVerifier) {
  const input = Buffer.concat([len8(Buffer.from(pw)), len8(Buffer.from(idProver)), len8(Buffer.from(idVerifier))])
  const out = crypto.pbkdf2Sync(input, salt, iterations, 80, 'sha256')
  const w0 = mod(big(out.subarray(0, 40)), n)
  const w1 = mod(big(out.subarray(40)), n)
  return { input, w0, w1, L: mul(w1, G) }
}

export const shareP = (x, w0) => add(mul(x, G), mul(w0, M))
export const shareV = (y, w0) => add(mul(y, G), mul(w0, N))

/// Both ends' derivations, given the shares; checks that they agree.
export function exchange({ context, idProver, idVerifier, w0, w1, L, x, y }) {
  const X = shareP(x, w0); const Y = shareV(y, w0)
  // Verifier
  const Zv = mul(y, add(X, neg(mul(w0, M)))); const Vv = mul(y, L)
  // Prover
  const Yn = add(Y, neg(mul(w0, N)))
  const Zp = mul(x, Yn); const Vp = mul(w1, Yn)
  if (enc(Zv).compare(enc(Zp)) || enc(Vv).compare(enc(Vp))) throw new Error('Z or V disagree')
  const TT = Buffer.concat([
    len8(Buffer.from(context)), len8(Buffer.from(idProver)), len8(Buffer.from(idVerifier)),
    len8(enc(M)), len8(enc(N)), len8(enc(X)), len8(enc(Y)), len8(enc(Zv)), len8(enc(Vv)), len8(be(w0, 32)),
  ])
  const Kmain = sha256(TT)
  const conf = hkdf(Buffer.alloc(0), Kmain, 'ConfirmationKeys', 64)
  const KconfirmP = conf.subarray(0, 32); const KconfirmV = conf.subarray(32)
  const Kshared = hkdf(Buffer.alloc(0), Kmain, 'SharedKey', 32)
  return {
    X: enc(X), Y: enc(Y), Z: enc(Zv), V: enc(Vv), TT, Kmain, KconfirmP, KconfirmV, Kshared,
    confirmP: hmac(KconfirmP, enc(Y)), confirmV: hmac(KconfirmV, enc(X)),
  }
}

export { add, be, big, neg, sha256 }
