/// The interface map as the device stores it, and the change set an operator
/// builds on top of it before one write sends the whole map.

export type IfaceKind = 'physical' | 'vlan' | 'bridge' | 'wireguard' | 'can'
/// What the console shows an interface as: a declared kind, or a radio the
/// device reports and nobody declared.
export type ViewKind = IfaceKind | 'wifi'

/// `presharedKey` reads as the redaction sentinel; sent back, it keeps the
/// stored key.
export interface WireguardPeer { name?: string; publicKey: string; presharedKey?: string; allowedIps: string[]; endpoint?: string; persistentKeepalive?: number }
export interface StaticRoute { destination: string; gateway?: string; metric?: number }
/// A CAN controller's parameters; no restart delay restarts nothing, and no FD is FD off.
export interface CanConfig { bitrate: number; restartMs?: number; fd?: boolean; dataBitrate?: number }

export interface IfaceEntry {
  kind?: IfaceKind
  dhcp?: boolean
  static?: { address: string; gateway?: string; dns?: string[] }
  /// A DHCP entry's own servers, used instead of the lease's.
  dns?: string[]
  vlan?: { parent: string; id: number }
  bridge?: { ports: string[] }
  wireguard?: { listenPort?: number; peers: WireguardPeer[] }
  can?: CanConfig
  routes?: StaticRoute[]
}

export type NetworkMap = Record<string, IfaceEntry>

/// `base` is the map the device stored when staging began; `staged` is what
/// one apply would write.
export interface ChangeSet { base: NetworkMap; staged: NetworkMap }

export type ChangeOp = 'create' | 'update' | 'delete'
export interface Change { name: string; op: ChangeOp; kind: ViewKind }

const VIRTUAL: ViewKind[] = ['vlan', 'bridge', 'wireguard']

export const isVirtual = (kind: ViewKind) => VIRTUAL.includes(kind)

const clone = <T>(value: T): T => structuredClone(value)

/// An entry without the members that mean nothing when empty, so that two
/// spellings of one configuration compare equal.
export function normalize(entry: IfaceEntry): IfaceEntry {
  const out: IfaceEntry = {}
  if (entry.kind && entry.kind !== 'physical') out.kind = entry.kind
  if (entry.dhcp) out.dhcp = true
  if (entry.static) {
    out.static = { address: entry.static.address }
    if (entry.static.gateway) out.static.gateway = entry.static.gateway
    if (entry.static.dns?.length) out.static.dns = [...entry.static.dns]
  }
  if (entry.dns?.length) out.dns = [...entry.dns]
  if (entry.vlan) out.vlan = { ...entry.vlan }
  if (entry.bridge) out.bridge = { ports: [...entry.bridge.ports] }
  if (entry.wireguard) out.wireguard = clone(entry.wireguard)
  if (entry.can) out.can = { ...entry.can }
  if (entry.routes?.length) out.routes = entry.routes.map((route) => ({ ...route }))
  return out
}

function sorted(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sorted)
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, sorted((value as Record<string, unknown>)[key])]))
  }
  return value
}

export function sameEntry(left: IfaceEntry | undefined, right: IfaceEntry | undefined) {
  if (left === undefined || right === undefined) return left === right
  return JSON.stringify(sorted(normalize(left))) === JSON.stringify(sorted(normalize(right)))
}

/// What an interface nobody declared runs as: the image's DHCP default.
export function defaultEntry(kind: ViewKind): IfaceEntry {
  if (kind === 'can') return { kind: 'can' }
  return { dhcp: true }
}

/// What an interface is once a bridge claims it: declared, with no addressing
/// of its own.
export function portEntry(current: IfaceEntry | undefined): IfaceEntry {
  const entry: IfaceEntry = { ...current, dhcp: false }
  delete entry.static
  delete entry.dns
  delete entry.routes
  return entry
}

/// Change one interface. A bridge's ports follow its port list: a port that
/// joins loses its own addressing, and one that leaves goes back to DHCP. A
/// VLAN's parent is declared if it was not, since the device refuses a VLAN
/// on an undeclared parent.
export function stageSet(set: ChangeSet, name: string, kind: ViewKind, change: (entry: IfaceEntry) => IfaceEntry): ChangeSet {
  const current = set.staged[name] ?? defaultEntry(kind)
  const next = change(clone(current))
  if (set.staged[name] !== undefined && sameEntry(current, next)) return set
  if (set.staged[name] === undefined && sameEntry(defaultEntry(kind), next)) return set
  const staged = { ...set.staged, [name]: next }
  if (next.kind === 'bridge') {
    const was = current.bridge?.ports ?? []
    const now = next.bridge?.ports ?? []
    for (const port of now.filter((each) => !was.includes(each))) staged[port] = portEntry(staged[port])
    for (const port of was.filter((each) => !now.includes(each))) {
      if (staged[port]) staged[port] = { ...staged[port], dhcp: true }
    }
  }
  const parent = next.vlan?.parent
  if (next.kind === 'vlan' && parent && staged[parent] === undefined) staged[parent] = { dhcp: true }
  return { ...set, staged }
}

export interface CreateSpec {
  kind: 'bridge' | 'vlan' | 'wireguard'
  name: string
  parent?: string
  vlanId?: number
  address?: string
  listenPort?: number
}

/// A new virtual interface, with DHCP until its page says otherwise; a tunnel
/// is its address and port, and has no DHCP client.
export function stageCreate(set: ChangeSet, spec: CreateSpec): ChangeSet {
  const entry: IfaceEntry = spec.kind === 'bridge'
    ? { kind: 'bridge', dhcp: true, bridge: { ports: [] } }
    : spec.kind === 'vlan'
      ? { kind: 'vlan', dhcp: true, vlan: { parent: spec.parent ?? '', id: spec.vlanId ?? 0 } }
      : { kind: 'wireguard', static: { address: spec.address ?? '' }, wireguard: { listenPort: spec.listenPort, peers: [] } }
  return stageSet(set, spec.name, spec.kind, () => entry)
}

/// Remove an interface from the map. A NIC or a CAN controller goes back to
/// the image's default; anything else is deleted, and a bridge first lets its
/// ports go.
export function stageRemove(set: ChangeSet, name: string, kind: ViewKind): ChangeSet {
  let next = set
  if (kind === 'bridge') next = stageSet(next, name, kind, (entry) => ({ ...entry, bridge: { ports: [] } }))
  const staged = { ...next.staged }
  delete staged[name]
  return { ...next, staged }
}

/// Put one interface back to what the device stores.
export function stageRevert(set: ChangeSet, name: string): ChangeSet {
  const staged = { ...set.staged }
  if (set.base[name] === undefined) delete staged[name]
  else staged[name] = clone(set.base[name])
  return { ...set, staged }
}

export function discardAll(set: ChangeSet): ChangeSet {
  return { base: set.base, staged: clone(set.base) }
}

/// The interfaces whose staged entry differs from the stored one, in `order`
/// and then by name.
export function pendingChanges(set: ChangeSet, kindOf: (name: string) => ViewKind, order: string[] = []): Change[] {
  const names = [...new Set([...Object.keys(set.base), ...Object.keys(set.staged)])]
  const rank = (name: string) => { const at = order.indexOf(name); return at === -1 ? Number.MAX_SAFE_INTEGER : at }
  names.sort((left, right) => rank(left) - rank(right) || left.localeCompare(right))
  const changes: Change[] = []
  for (const name of names) {
    const before = set.base[name]
    const after = set.staged[name]
    const kind = after?.kind ?? before?.kind ?? kindOf(name)
    if (before === undefined && after !== undefined) changes.push({ name, op: isVirtual(kind) ? 'create' : 'update', kind })
    else if (before !== undefined && after === undefined) changes.push({ name, op: isVirtual(kind) ? 'delete' : 'update', kind })
    else if (!sameEntry(before, after)) changes.push({ name, op: 'update', kind })
  }
  return changes
}

/// Whether applying `changes` can cut the browser off: `session` is the
/// interface the console is reached through, and `null` when that cannot be
/// told, in which case every change set is treated as touching it.
export function touchesSession(changes: Change[], set: ChangeSet, session: string | null): boolean {
  if (changes.length === 0) return false
  if (session === null) return true
  const around = new Set([session])
  for (const map of [set.base, set.staged]) {
    const own = map[session]
    if (own?.vlan?.parent) around.add(own.vlan.parent)
    for (const port of own?.bridge?.ports ?? []) around.add(port)
    for (const [name, entry] of Object.entries(map)) {
      if (entry.bridge?.ports.includes(session)) around.add(name)
    }
  }
  return changes.some((change) => around.has(change.name))
}

/// The bridge whose ports include `name`.
export function bridgeOf(map: NetworkMap, name: string): string | undefined {
  return Object.entries(map).find(([, entry]) => entry.kind === 'bridge' && entry.bridge?.ports.includes(name))?.[0]
}

/// The VLANs whose parent is `name`.
export function vlansOn(map: NetworkMap, name: string): string[] {
  return Object.entries(map).filter(([, entry]) => entry.kind === 'vlan' && entry.vlan?.parent === name).map(([vlan]) => vlan).sort()
}

/// Copy every tunnel's peer list from a fresh read of the map: peers are
/// written straight away, not staged, and the one write replaces them.
export function withFreshPeers(staged: NetworkMap, fresh: NetworkMap): NetworkMap {
  const out = clone(staged)
  for (const [name, entry] of Object.entries(out)) {
    const peers = fresh[name]?.wireguard?.peers
    if (entry.wireguard && peers) entry.wireguard.peers = clone(peers)
  }
  return out
}

/// Take a fresh read of the stored map. Entries the change set does not touch
/// follow the device; an entry it touches that moved underneath it is a
/// conflict, and the set is left as it was.
export function rebase(set: ChangeSet, fresh: NetworkMap): { set: ChangeSet; conflicts: string[] } {
  const conflicts: string[] = []
  const base = clone(set.base)
  const staged = clone(set.staged)
  for (const name of new Set([...Object.keys(base), ...Object.keys(staged), ...Object.keys(fresh)])) {
    const touched = !sameEntry(base[name], staged[name])
    const moved = !sameEntry(base[name], fresh[name])
    if (!moved) continue
    if (touched) {
      // A peer added since staging began moves the stored entry, and peers
      // are not part of the change set.
      const peersOnly = base[name] && fresh[name] && sameEntry(withPeers(base[name], fresh[name]), fresh[name])
      if (peersOnly) { base[name] = clone(fresh[name]); if (staged[name]) staged[name] = withPeers(staged[name], fresh[name]) }
      else conflicts.push(name)
      continue
    }
    if (fresh[name] === undefined) { delete base[name]; delete staged[name] } else { base[name] = clone(fresh[name]); staged[name] = clone(fresh[name]) }
  }
  if (conflicts.length > 0) return { set, conflicts }
  return { set: { base, staged }, conflicts }
}

function withPeers(entry: IfaceEntry, from: IfaceEntry): IfaceEntry {
  if (!entry.wireguard || !from.wireguard) return entry
  return { ...entry, wireguard: { ...entry.wireguard, peers: clone(from.wireguard.peers) } }
}

// ---------- IPv4 ----------

export function isIPv4(text: string | undefined): boolean {
  return /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/.test(text ?? '')
}

export function ipToInt(ip: string): number {
  return ip.split('.').map(Number).reduce((acc, octet) => acc * 256 + octet, 0)
}

export function intToIp(value: number): string {
  return [24, 16, 8, 0].map((shift) => Math.floor(value / 2 ** shift) % 256).join('.')
}

export interface Cidr { ip: number; prefix: number; net: number; broadcast: number }

export function parseCidr(text: string | undefined): Cidr | undefined {
  const match = /^([\d.]+)\/(\d{1,2})$/.exec((text ?? '').trim())
  if (!match || !isIPv4(match[1]) || Number(match[2]) > 32) return undefined
  const prefix = Number(match[2])
  const ip = ipToInt(match[1])
  const size = 2 ** (32 - prefix)
  const net = Math.floor(ip / size) * size
  return { ip, prefix, net, broadcast: net + size - 1 }
}

/// Whether `ip` is a host address of the subnet `cidr`.
export function inSubnet(ip: string, cidr: string | undefined): boolean {
  const subnet = parseCidr(cidr)
  if (!subnet || !isIPv4(ip)) return false
  const value = ipToInt(ip)
  return value > subnet.net && value < subnet.broadcast
}

export function overlaps(left: string, right: string): boolean {
  const a = parseCidr(left)
  const b = parseCidr(right)
  if (!a || !b) return false
  return a.net <= b.broadcast && b.net <= a.broadcast
}

export const NAME_PATTERN = /^[a-zA-Z][a-zA-Z0-9_-]{0,14}$/

/// The first `<prefix><n>` from `from` that names no interface.
export function nextName(taken: (name: string) => boolean, prefix: string, from: number): string {
  for (let at = from; at < 100; at++) if (!taken(`${prefix}${at}`)) return `${prefix}${at}`
  return `${prefix}99`
}
