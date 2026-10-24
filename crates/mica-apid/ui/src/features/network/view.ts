import { isHiddenInterface } from '@/lib/network'
import type { BoardPort, ObservedNetworkInterface, ObservedNetworkState } from '@/lib/types'
import type { StatusTone } from '@/shared/components/status-badge'
import { bridgeOf, carriesInternet, defaultEntry, overlaps, usesInternet, vlansOn, type IfaceEntry, type NetworkMap, type ViewKind } from './model'

/// What the device reports about one interface, in the terms the pages use.
export interface Observed {
  carrier: boolean
  operationalState?: string
  /// The first IPv4 address with its prefix.
  address?: string
  /// Where that address came from, as networkd says it (`DHCPv4`, `static`).
  source?: string
  mac?: string
  mtu?: number
  lease?: { server?: string; router?: string; remainingSeconds?: number }
  dns: string[]
  /// The CAN controller's state, as the kernel reports it.
  can?: { busState?: string; bitrate?: number; txErrors?: number; rxErrors?: number; busOffCount?: number }
  /// Seconds since each tunnel peer's latest handshake, `null` for a peer
  /// that never completed one. Absent when the device did not say.
  handshakeAges?: (number | null)[]
  /// The IPv4 routes of the main table on the link.
  routes: ObservedRoute[]
}

type ObservedRoute = NonNullable<ObservedNetworkInterface['routes']>[number]

export interface IfaceView {
  name: string
  kind: ViewKind
  declared: boolean
  /// The entry the interface runs, or would run: the stored or staged one, or
  /// the image's default when nobody declared it.
  entry: IfaceEntry
  /// Absent when the device does not report the interface.
  obs?: Observed
  /// What the board declares about the port, when it does.
  port?: BoardPort
}

export interface WifiView {
  radio?: string
  mode: 'off' | 'client' | 'ap'
  apRunning: boolean
  /// Why the access point is not running, or `acs_failed` while it runs on
  /// the fallback channel, as the device says.
  apReason?: string
  apSsid?: string
  /// The channel the access point runs on, as hostapd reports it.
  apChannel?: number
  apFrequencyMhz?: number
  stations: number
  association?: { ssid?: string; bssid?: string; frequencyMhz?: number; keyManagement?: string; rssiDbm?: number; linkSpeedMbps?: number }
}

export interface DefaultRoute {
  gateway?: string
  interface: string
  metric?: number
  /// Whether the kernel sends traffic for the internet this way; absent when
  /// the device did not say.
  effective?: boolean
}

/// One row of the routes tab's route table.
export interface RouteRow {
  destination: string
  prefix: number
  via?: string
  interface: string
  metric?: number
  /// Who installed it: a DHCP lease, the configuration, a tunnel, or the
  /// kernel for an address's own subnet.
  source: 'dhcp' | 'static' | 'wireguard' | 'link'
  /// Set on default routes when there is more than one: the one in use and
  /// the ones standing by.
  role?: 'effective' | 'standby'
}

export interface NetView {
  map: NetworkMap
  ifaces: Record<string, IfaceView>
  order: string[]
  /// Whether the observation was read; before it is, nothing is "missing".
  observed: boolean
  /// Whether the board declares its ports, so a port it does not is noted.
  boardDeclared: boolean
  routes: DefaultRoute[]
  wifi: WifiView
}

/// A WireGuard peer whose latest handshake is older than this has no session.
const HANDSHAKE_FRESH_SECONDS = 180

function observedOf(entry: ObservedNetworkInterface, now: number): Observed {
  const ipv4 = entry.addresses.find((address) => (address.family === 'ipv4' || address.family === 'inet' || (address.address ?? '').includes('.')) && address.scope !== 'host' && address.address)
  const lease = entry.lease?.available ? entry.lease : undefined
  const can = entry.can?.available ? entry.can : undefined
  const peers = entry.wireguard?.available ? entry.wireguard.peers ?? [] : undefined
  return {
    carrier: entry.link.carrier === true,
    operationalState: entry.link.operationalState,
    address: ipv4?.address ? `${ipv4.address}${ipv4.prefixLength === undefined ? '' : `/${ipv4.prefixLength}`}` : undefined,
    source: ipv4?.configSource,
    mac: entry.hardwareAddress,
    mtu: entry.mtu,
    lease: lease ? { server: lease.server, router: lease.router, remainingSeconds: lease.remainingSeconds } : undefined,
    dns: entry.linkDns
      ? entry.linkDns.map((server) => server.address)
      : entry.dns.map((server) => typeof server === 'string' ? server : server.address ?? '').filter(Boolean),
    can: can ? { busState: can.busState, bitrate: can.bitrate, txErrors: can.txErrors, rxErrors: can.rxErrors, busOffCount: can.busOffCount } : undefined,
    handshakeAges: peers?.map((peer) => peer.latestHandshake ? Math.max(0, Math.floor(now / 1000) - peer.latestHandshake) : null),
    routes: (entry.routes ?? []).filter((route) =>
      (route.table === undefined || route.table === 'main' || route.table === 254)
      && (route.family === undefined ? !route.dest.includes(':') : route.family === 'ipv4' || route.family === 'inet')),
  }
}

/// The kind an interface nobody declared is shown as.
function observedKind(entry: ObservedNetworkInterface, radios: string[]): ViewKind {
  if (entry.type === 'wlan' || radios.includes(entry.name)) return 'wifi'
  if (entry.type === 'can') return 'can'
  if (entry.kind === 'bridge' || entry.kind === 'vlan' || entry.kind === 'wireguard') return entry.kind
  return 'physical'
}

const KIND_RANK: Record<ViewKind, number> = { physical: 0, bridge: 1, vlan: 2, wifi: 3, wireguard: 4, can: 5 }

const PORT_KIND: Record<BoardPort['kind'], ViewKind> = { ethernet: 'physical', wifi: 'wifi', can: 'can' }

/// Whether `name` stays off the internet while its entry does not say: the
/// board declares it internal.
export const internalPort = (view: NetView, name: string) => view.ifaces[name]?.port?.role === 'internal'

/// A physical port the board does not declare, on a board that declares its
/// ports.
export const undeclaredPort = (view: NetView, name: string) => view.boardDeclared && view.ifaces[name]?.kind === 'physical' && !view.ifaces[name]?.port

/// Join a map with what the device observes. Interfaces the device creates
/// for itself (the loopback, container links) are left out unless declared.
/// The ports the board declares lead, in its order, present or not.
export function buildView(map: NetworkMap, status: ObservedNetworkState | undefined, wifi: WifiView, now = Date.now()): NetView {
  const radios = status?.capabilities.wifi.interfaces ?? []
  const entries = status?.interfaces.entries ?? []
  const ifaces: Record<string, IfaceView> = {}
  const index = new Map<string, number>()
  const ports = new Map(entries.filter((entry) => entry.declared).map((entry) => [entry.name, entry.declared as BoardPort]))
  for (const entry of entries) {
    if (entry.present === false) {
      const port = entry.declared
      if (port && map[entry.name] === undefined) ifaces[entry.name] = { name: entry.name, kind: PORT_KIND[port.kind], declared: false, entry: defaultEntry(PORT_KIND[port.kind]) }
      continue
    }
    if (map[entry.name] === undefined && isHiddenInterface(entry.name, entry.kind ?? entry.type)) continue
    const kind = map[entry.name]?.kind ?? observedKind(entry, radios)
    ifaces[entry.name] = { name: entry.name, kind, declared: map[entry.name] !== undefined, entry: map[entry.name] ?? defaultEntry(kind), obs: observedOf(entry, now) }
    index.set(entry.name, entry.index ?? Number.MAX_SAFE_INTEGER)
  }
  for (const [name, entry] of Object.entries(map)) {
    if (ifaces[name]) continue
    ifaces[name] = { name, kind: entry.kind ?? 'physical', declared: true, entry }
  }
  for (const [name, port] of ports) if (ifaces[name]) ifaces[name].port = port
  const rank = (name: string) => ports.get(name)?.order ?? Number.MAX_SAFE_INTEGER
  const order = Object.keys(ifaces).sort((left, right) =>
    rank(left) - rank(right)
    || KIND_RANK[ifaces[left].kind] - KIND_RANK[ifaces[right].kind]
    || (index.get(left) ?? Number.MAX_SAFE_INTEGER) - (index.get(right) ?? Number.MAX_SAFE_INTEGER)
    || left.localeCompare(right))
  const routes = (status?.defaultRoutes.entries ?? [])
    .filter((route): route is typeof route & { interface: string } => route.interface !== undefined && (route.family === undefined || route.family === 'ipv4' || route.family === 'inet'))
    .map((route) => ({ gateway: route.gateway, interface: route.interface, metric: route.metric, effective: route.effective }))
    .sort((left, right) => Number(right.effective === true) - Number(left.effective === true) || (left.metric ?? 0) - (right.metric ?? 0))
  return { map, ifaces, order, observed: status !== undefined, boardDeclared: status?.board === 'declared', routes, wifi }
}

/// The interface the default route leaves by: the one the kernel uses, or
/// the lowest metric when the device did not say.
export function exitOf(view: NetView): string | undefined {
  return view.routes[0]?.interface
}

/// Whether some interface may still carry the default route: one of a kind
/// that can (the radio only as a client), not a bridge port, that keeps
/// `internet`.
export function keepsInternet(view: NetView): boolean {
  return view.order.some((name) => {
    const iface = view.ifaces[name]
    if (iface.kind === 'wifi' && view.wifi.mode !== 'client') return false
    return carriesInternet(iface.kind) && !bridgeOf(view.map, name) && usesInternet(iface.entry, internalPort(view, name))
  })
}

function routeSource(iface: IfaceView, route: ObservedRoute): RouteRow['source'] {
  if (iface.kind === 'wireguard') return 'wireguard'
  if (route.protocol === 'kernel') return 'link'
  if (route.protocol === 'dhcp' || route.source === 'DHCPv4') return 'dhcp'
  if (route.protocol === 'static' || route.protocol === 'boot') return 'static'
  // A device that does not say who installed the route (OpenRC): a route
  // without a gateway is the link's own.
  if (route.via === undefined || route.via === null) return 'link'
  return route.source === 'static' ? 'static' : 'link'
}

/// The routes of the interfaces the pages list: default routes first, the one
/// in use leading, then each interface's in page order, narrowest first.
export function routeTable(view: NetView): RouteRow[] {
  const defaults: RouteRow[] = []
  const others: RouteRow[] = []
  view.order.forEach((name) => {
    const iface = view.ifaces[name]
    for (const route of iface.obs?.routes ?? []) {
      const [destination, prefix] = route.dest.split('/')
      const row: RouteRow = { destination: route.dest, prefix: Number(prefix ?? 32), via: route.via ?? undefined, interface: name, metric: route.metric ?? undefined, source: routeSource(iface, route) }
      if (destination === '0.0.0.0' && row.prefix === 0) defaults.push(row)
      else others.push(row)
    }
  })
  const rank = (row: RouteRow) => {
    const at = view.routes.findIndex((route) => route.interface === row.interface && (route.gateway === undefined || route.gateway === row.via))
    return at < 0 ? view.routes.length : at
  }
  defaults.sort((left, right) => rank(left) - rank(right) || (left.metric ?? 0) - (right.metric ?? 0))
  if (defaults.length > 1) defaults.forEach((row, at) => { row.role = at === 0 ? 'effective' : 'standby' })
  // Array sort is stable, so rows keep the interface order between equal prefixes.
  others.sort((left, right) => view.order.indexOf(left.interface) - view.order.indexOf(right.interface) || right.prefix - left.prefix)
  return [...defaults, ...others]
}

export interface IfaceStatus {
  tone: StatusTone
  /// A key under `network.status`.
  label: string
  note?: { key: string; params?: Record<string, string | number> }
}

const status = (tone: StatusTone, label: string, note?: IfaceStatus['note']): IfaceStatus => ({ tone, label, ...(note ? { note } : {}) })

/// An interface's state, by its kind: one rule for the overview, the list
/// and its page.
export function computeStatus(view: NetView, name: string): IfaceStatus {
  const iface = view.ifaces[name]
  if (!iface) return status('neutral', 'pendingCreate', { key: 'network.status.notes.afterApply' })
  if (!view.observed) return status('neutral', 'unknown')
  const obs = iface.obs
  if (!obs) {
    const hardware = iface.kind === 'physical' || iface.kind === 'can' || iface.kind === 'wifi'
    const note = hardware ? 'deviceMissing' : iface.kind === 'vlan' ? 'parentMissing' : 'createFailed'
    return status('danger', hardware ? 'notDetected' : 'notCreated', { key: `network.status.notes.${note}`, params: { parent: iface.entry.vlan?.parent ?? '' } })
  }
  const entry = iface.entry
  switch (iface.kind) {
    case 'physical': {
      const bridge = bridgeOf(view.map, name)
      if (bridge) return status(obs.carrier ? 'success' : 'danger', obs.carrier ? 'bridgePort' : 'noCable', { key: 'network.status.notes.memberOf', params: { bridge } })
      if (!obs.carrier) return status('danger', 'noCable')
      const vlans = vlansOn(view.map, name)
      if (!obs.address && vlans.length) return status('success', 'cabled', { key: 'network.status.notes.carriesVlan', params: { vlans: vlans.join(', ') } })
      if (!obs.address) return status('warning', 'cabled', { key: entry.dhcp ? 'network.status.notes.acquiring' : 'network.status.notes.noAddress' })
      return status('success', 'connected', exitOf(view) === name ? { key: 'network.status.notes.exit' } : undefined)
    }
    case 'bridge': {
      const ports = entry.bridge?.ports ?? []
      const up = ports.filter((port) => view.ifaces[port]?.obs?.carrier).length
      const fraction = { key: 'network.status.notes.portsUp', params: { up, count: ports.length } }
      if (!ports.length) return status('neutral', 'emptyBridge', { key: 'network.status.notes.noPorts' })
      if (up === 0) return status('danger', 'portsDown', fraction)
      if (up < ports.length) return status('warning', 'partlyUp', fraction)
      if (obs.address) return status('success', 'connected', fraction)
      if (entry.dhcp) return status('warning', 'acquiring', fraction)
      return status('success', 'forwarding', { key: 'network.status.notes.forwarding', params: { up, count: ports.length } })
    }
    case 'vlan': {
      const parent = entry.vlan?.parent ?? ''
      const params = { id: entry.vlan?.id ?? 0, parent }
      if (!view.ifaces[parent]?.obs?.carrier) return status('danger', 'parentDown', { key: 'network.status.notes.parent', params })
      if (!obs.address) return status('warning', 'noAddress', { key: 'network.status.notes.vlan', params })
      return status('success', 'connected', { key: 'network.status.notes.vlan', params })
    }
    case 'wireguard': {
      const peers = entry.wireguard?.peers.length ?? 0
      if (!peers) return status('neutral', 'noPeers')
      const ages = obs.handshakeAges
      if (!ages) return status('warning', 'started', { key: 'network.status.notes.peersUnknown', params: { count: peers } })
      const seen = ages.filter((age) => age !== null)
      const handshakeAge = seen.length ? Math.min(...seen) : undefined
      const up = seen.filter((age) => age <= HANDSHAKE_FRESH_SECONDS).length
      if (up > 0) return status('success', 'tunnelUp', { key: 'network.status.notes.peersOnline', params: { up, count: ages.length, handshakeAge: handshakeAge ?? 0 } })
      // A peer with keepalive handshakes every two minutes even when idle, so
      // its silence means it is gone; without keepalive, silence is no traffic.
      const keepalive = (entry.wireguard?.peers ?? []).some((peer) => peer.persistentKeepalive)
      if (keepalive) return status('danger', 'peersOffline', handshakeAge === undefined ? { key: 'network.status.notes.neverHandshake' } : { key: 'network.status.notes.lastHandshake', params: { handshakeAge } })
      return status('warning', 'idle', handshakeAge === undefined ? { key: 'network.status.notes.idleNever' } : { key: 'network.status.notes.idleHandshake', params: { handshakeAge } })
    }
    case 'wifi': {
      const wifi = view.wifi
      if (wifi.radio !== undefined && wifi.radio !== name) return status('neutral', 'off')
      if (wifi.mode === 'off') return status('neutral', 'off')
      if (wifi.mode === 'ap') {
        if (!wifi.apRunning) return status('danger', 'apDown', wifi.apReason ? { key: `network.status.notes.apReason.${wifi.apReason}` } : undefined)
        if (wifi.apReason === 'acs_failed') return status('warning', 'apRunning', { key: 'network.status.notes.apFallback', params: { ssid: wifi.apSsid ?? '' } })
        return status('success', 'apRunning', { key: 'network.status.notes.ap', params: { ssid: wifi.apSsid ?? '', count: wifi.stations } })
      }
      if (!wifi.association) return status('danger', 'notConnected', { key: 'network.status.notes.searching' })
      return status('success', 'connected', { key: 'network.status.notes.joined', params: { ssid: wifi.association.ssid ?? '' } })
    }
    case 'can': {
      const can = obs.can
      if (!can) return status('success', 'running', { key: 'network.status.notes.bitrateUnreported' })
      const bitrate = { key: 'network.status.notes.bitrate', params: { kbps: (can.bitrate ?? 0) / 1000 } }
      if (can.busState === 'bus-off') return status('danger', 'busOff', bitrate)
      if (can.busState === 'error-passive' || can.busState === 'error-warning') return status('warning', 'busErrors', bitrate)
      if (can.busState === 'stopped' || can.busState === 'sleeping') return status('neutral', 'off')
      return status('success', 'running', bitrate)
    }
  }
}

/// The interface the browser reached the console through: the one that holds
/// the address the connection was accepted on, as the device answers it, or
/// as found from the session's local address. `null` when no interface does,
/// as behind a proxy, or when the device did not say.
export function sessionInterface(status: ObservedNetworkState | undefined, localAddress: string | undefined): string | null {
  if (status?.session) return status.session.interface
  if (!localAddress || !status) return null
  const found = (status.interfaces.entries ?? []).find((entry) => entry.addresses.some((address) => address.address === localAddress))
  return found?.name ?? null
}

/// The listen ports a new tunnel is offered, in order.
const TUNNEL_PORTS = Array.from({ length: 79 }, (_, at) => 51821 + at)
/// The tunnel addresses a new tunnel is offered, in order.
const TUNNEL_ADDRESSES = [
  ...Array.from({ length: 253 }, (_, at) => `10.${at + 2}.0.1/24`),
  ...Array.from({ length: 156 }, (_, at) => `172.16.${at + 100}.1/24`),
]

/// What a new tunnel starts from: the first name, subnet and listen port that
/// nothing in `view` has (staged changes included) and, for the port, no
/// service on the device holds (`held`). A value with no free candidate is
/// absent.
export function tunnelDefaults(view: NetView, held: number[]): { name?: string; address?: string; listenPort?: number } {
  const entries = Object.values(view.map)
  // A tunnel the device runs and the map no longer declares is one a staged
  // change deletes: what it holds is free.
  const live = view.order.filter((name) => view.ifaces[name].declared || view.ifaces[name].kind !== 'wireguard')
  const subnets = [
    ...live.flatMap((name) => [view.ifaces[name].obs?.address, ...(view.ifaces[name].obs?.routes ?? []).filter((route) => !route.dest.endsWith('/0')).map((route) => route.dest)]),
    ...entries.flatMap((entry) => [entry.dhcp ? undefined : entry.static?.address, ...(entry.wireguard?.peers ?? []).flatMap((peer) => peer.allowedIps)]),
  ].filter((subnet): subnet is string => subnet !== undefined)
  const ports = new Set([...held, ...entries.flatMap((entry) => entry.wireguard?.listenPort ?? [])])
  return {
    name: Array.from({ length: 100 }, (_, at) => `wg${at}`).find((name) => !live.includes(name)),
    address: TUNNEL_ADDRESSES.find((address) => !subnets.some((subnet) => overlaps(address, subnet))),
    listenPort: TUNNEL_PORTS.find((port) => !ports.has(port)),
  }
}
