// RFC 9383 Appendix C, P256-SHA256-HKDF-HMAC-SHA256.
import { exchange, mul, enc } from './spake2plus.mjs'

const G = [0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296n, 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5n]
const w0 = 0xbb8e1bbcf3c48f62c08db243652ae55d3e5586053fca77102994f23ad95491b3n
const w1 = 0x7e945f34d78785b8a3ef44d0df5a1a97d6b3b460409a345ca7830387a74b1dban
const L = mul(w1, G)
const out = exchange({
  context: 'SPAKE2+-P256-SHA256-HKDF-SHA256-HMAC-SHA256 Test Vectors',
  idProver: 'client', idVerifier: 'server', w0, w1, L,
  x: 0xd1232c8e8693d02368976c174e2088851b8365d0d79a9eee709c6a05a2fad539n,
  y: 0x717a72348a182085109c8d3917d6c43d59b224dc6a7fc4f0483232fa6516d8b3n,
})
const want = {
  L: '04eb7c9db3d9a9eb1f8adab81b5794c1f13ae3e225efbe91ea487425854c7fc00f00bfedcbd09b2400142d40a14f2064ef31dfaa903b91d1faea7093d835966efd',
  X: '04ef3bd051bf78a2234ec0df197f7828060fe9856503579bb1733009042c15c0c1de127727f418b5966afadfdd95a6e4591d171056b333dab97a79c7193e341727',
  Y: '04c0f65da0d11927bdf5d560c69e1d7d939a05b0e88291887d679fcadea75810fb5cc1ca7494db39e82ff2f50665255d76173e09986ab46742c798a9a68437b048',
  Z: '04bbfce7dd7f277819c8da21544afb7964705569bdf12fb92aa388059408d50091a0c5f1d3127f56813b5337f9e4e67e2ca633117a4fbd559946ab474356c41839',
  V: '0458bf27c6bca011c9ce1930e8984a797a3419797b936629a5a937cf2f11c8b9514b82b993da8a46e664f23db7c01edc87faa530db01c2ee405230b18997f16b68',
  Kmain: '4c59e1ccf2cfb961aa31bd9434478a1089b56cd11542f53d3576fb6c2a438a29',
  KconfirmP: '871ae3f7b78445e34438fb284504240239031c39d80ac23eb5ab9be5ad6db58a',
  KconfirmV: 'ccd53c7c1fa37b64a462b40db8be101cedcf838950162902054e644b400f1680',
  confirmP: '926cc713504b9b4d76c9162ded04b5493e89109f6d89462cd33adc46fda27527',
  confirmV: '9747bcc4f8fe9f63defee53ac9b07876d907d55047e6ff2def2e7529089d3e68',
  Kshared: '0c5f8ccd1413423a54f6c1fb26ff01534a87f893779c6e68666d772bfd91f3e7',
}
const got = { ...out, L: enc(L) }
let ok = true
for (const [k, v] of Object.entries(want)) {
  const g = got[k].toString('hex')
  if (g !== v) { ok = false; console.log('MISMATCH', k, g) }
}
console.log(ok ? 'RFC 9383 P-256 vector: all values match' : 'FAILED')
process.exit(ok ? 0 : 1)
