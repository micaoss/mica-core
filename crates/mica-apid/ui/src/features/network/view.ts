import { isHiddenInterface } from '@/lib/network'
import type { ObservedNetworkInterface, ObservedNetworkState } from '@/lib/types'
import type { StatusTone } from '@/shared/components/status-badge'
import { bridgeOf, defaultEntry, vlansOn, type IfaceEntry, type NetworkMap, type ViewKind } from './model'

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
}

export interface IfaceView {
  name: string
  kind: ViewKind
  declared: boolean
  /// The entry the interface runs, or would run: the stored or staged one, or
  /// the image's default when nobody declared it.
  entry: IfaceEntry
  /// Absent when the device does not report the interface.
  obs?: Observed
}

export interface WifiView {
  radio?: string
  mode: 'off' | 'client' | 'ap'
  apRunning: boolean
  /// Why the access point is not running, as the device says.
  apReason?: string
  apSsid?: string
  stations: number
  association?: { ssid?: string; bssid?: string; rssiDbm?: number; linkSpeedMbps?: number }
}

export interface DefaultRoute { gateway?: string; interface: string; metric?: number }

export interface NetView {
  map: NetworkMap
  ifaces: Record<string, IfaceView>
  order: string[]
  /// Whether the observation was read; before it is, nothing is "missing".
  observed: boolean
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

/// Join a map with what the device observes. Interfaces the device creates
/// for itself (the loopback, container links) are left out unless declared.
export function buildView(map: NetworkMap, status: ObservedNetworkState | undefined, wifi: WifiView, now = Date.now()): NetView {
  const radios = status?.capabilities.wifi.interfaces ?? []
  const entries = status?.interfaces.entries ?? []
  const ifaces: Record<string, IfaceView> = {}
  const index = new Map<string, number>()
  for (const entry of entries) {
    if (entry.present === false) continue
    if (map[entry.name] === undefined && isHiddenInterface(entry.name, entry.kind ?? entry.type)) continue
    const kind = map[entry.name]?.kind ?? observedKind(entry, radios)
    ifaces[entry.name] = { name: entry.name, kind, declared: map[entry.name] !== undefined, entry: map[entry.name] ?? defaultEntry(kind), obs: observedOf(entry, now) }
    index.set(entry.name, entry.index ?? Number.MAX_SAFE_INTEGER)
  }
  for (const [name, entry] of Object.entries(map)) {
    if (ifaces[name]) continue
    ifaces[name] = { name, kind: entry.kind ?? 'physical', declared: true, entry }
  }
  const order = Object.keys(ifaces).sort((left, right) =>
    KIND_RANK[ifaces[left].kind] - KIND_RANK[ifaces[right].kind]
    || (index.get(left) ?? Number.MAX_SAFE_INTEGER) - (index.get(right) ?? Number.MAX_SAFE_INTEGER)
    || left.localeCompare(right))
  const routes = (status?.defaultRoutes.entries ?? [])
    .filter((route): route is typeof route & { interface: string } => route.interface !== undefined && (route.family === undefined || route.family === 'ipv4' || route.family === 'inet'))
    .map((route) => ({ gateway: route.gateway, interface: route.interface, metric: route.metric }))
    .sort((left, right) => (left.metric ?? 0) - (right.metric ?? 0))
  return { map, ifaces, order, observed: status !== undefined, routes, wifi }
}

/// The interface the default route leaves by.
export function exitOf(view: NetView): string | undefined {
  return view.routes[0]?.interface
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
    const note = iface.kind === 'physical' || iface.kind === 'can' ? 'deviceMissing' : iface.kind === 'vlan' ? 'parentMissing' : 'createFailed'
    return status('danger', 'notCreated', { key: `network.status.notes.${note}`, params: { parent: iface.entry.vlan?.parent ?? '' } })
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
      const up = ages.filter((age) => age !== null && age <= HANDSHAKE_FRESH_SECONDS).length
      if (up === 0) return status('warning', 'started', { key: 'network.status.notes.noHandshake' })
      return status('success', 'connected', { key: 'network.status.notes.peersUp', params: { up, count: ages.length } })
    }
    case 'wifi': {
      const wifi = view.wifi
      if (wifi.radio !== undefined && wifi.radio !== name) return status('neutral', 'off')
      if (wifi.mode === 'off') return status('neutral', 'off')
      if (wifi.mode === 'ap') {
        if (!wifi.apRunning) return status('danger', 'apDown', wifi.apReason ? { key: `network.status.notes.apReason.${wifi.apReason}` } : undefined)
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
