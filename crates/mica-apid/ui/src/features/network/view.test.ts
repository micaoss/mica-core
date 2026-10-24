import { describe, expect, it } from 'vitest'
import { configured, observed } from '@/shared/testing/network'
import type { NetworkMap } from './model'
import { diffRows } from './summary'
import { wifiView } from './use-network'
import { addressBook, validateEntry } from './validate'
import { buildView, computeStatus, exitOf, internalPort, keepsInternet, routeTable, undeclaredPort, sessionInterface, tunnelDefaults, type WifiView } from './view'

const off: WifiView = { radio: 'wlan0', mode: 'off', apRunning: false, stations: 0 }
const t = (key: string, params?: Record<string, unknown>) => params ? `${key}${JSON.stringify(params)}` : key

describe('the joined view', () => {
  const view = buildView(configured() as NetworkMap, observed(), off)

  it('orders by kind and index and leaves the loopback out', () => {
    expect(view.order).toEqual(['eth0', 'eth1', 'eth2', 'wlan0', 'wg0', 'can0'])
    expect(view.ifaces.eth2.declared).toBe(false)
    expect(view.ifaces.wlan0.kind).toBe('wifi')
    expect(exitOf(view)).toBe('eth0')
  })

  it('reads each interface\'s state by its kind', () => {
    expect(computeStatus(view, 'eth0')).toEqual({ tone: 'success', label: 'connected', note: { key: 'network.status.notes.exit' } })
    expect(computeStatus(view, 'eth2').label).toBe('noCable')
    expect(computeStatus(view, 'wlan0').label).toBe('off')
    expect(computeStatus(view, 'wg0').label).toBe('started')
    expect(computeStatus(view, 'br9').label).toBe('pendingCreate')
  })

  it('tells a bridge with some ports down and a VLAN whose parent is down', () => {
    const map: NetworkMap = { ...configured() as NetworkMap, br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth1', 'eth2'] } }, 'eth2-5': { kind: 'vlan', dhcp: true, vlan: { parent: 'eth2', id: 5 } } }
    const status = observed()
    status.interfaces.entries!.push(
      { name: 'br0', index: 8, kind: 'bridge', link: { carrier: true }, addresses: [], dhcp: { available: true }, dns: [] },
      { name: 'eth2-5', index: 9, kind: 'vlan', link: { carrier: false }, addresses: [], dhcp: { available: true }, dns: [] },
    )
    const joined = buildView(map, status, off)
    expect(computeStatus(joined, 'br0').label).toBe('partlyUp')
    expect(computeStatus(joined, 'eth2-5').label).toBe('parentDown')
    expect(computeStatus(joined, 'eth1').label).toBe('bridgePort')
  })

  it('reads a CAN controller\'s bus state and a tunnel\'s handshakes', () => {
    const status = observed()
    const now = 1_800_000_000_000
    for (const entry of status.interfaces.entries!) {
      if (entry.name === 'can0') entry.can = { available: true, busState: 'error-passive', bitrate: 500000, txErrors: 130, rxErrors: 0 }
      if (entry.name === 'wg0') entry.wireguard = { available: true, peers: [{ publicKey: 'a', latestHandshake: now / 1000 - 30 }, { publicKey: 'b', latestHandshake: null }] }
    }
    const map: NetworkMap = { ...configured() as NetworkMap, wg0: { kind: 'wireguard', static: { address: '10.8.0.1/24' }, wireguard: { peers: [{ publicKey: 'a' }, { publicKey: 'b' }] } } as NetworkMap[string] }
    const joined = buildView(map, status, off, now)
    expect(computeStatus(joined, 'can0')).toEqual({ tone: 'warning', label: 'busErrors', note: { key: 'network.status.notes.bitrate', params: { kbps: 500 } } })
    expect(computeStatus(joined, 'wg0')).toEqual({ tone: 'success', label: 'tunnelUp', note: { key: 'network.status.notes.peersOnline', params: { up: 1, count: 2, handshakeAge: 30 } } })
  })

  it('tells a tunnel whose peers stopped answering from one that is only quiet', () => {
    const now = 1_800_000_000_000
    const tunnel = (keepalive: number | undefined, handshake: number | null) => {
      const status = observed()
      for (const entry of status.interfaces.entries!) {
        if (entry.name === 'wg0') entry.wireguard = { available: true, peers: [{ publicKey: 'a', latestHandshake: handshake }] }
      }
      const map: NetworkMap = { ...configured() as NetworkMap, wg0: { kind: 'wireguard', static: { address: '10.8.0.1/24' }, wireguard: { peers: [{ publicKey: 'a', allowedIps: ['10.8.0.2/32'], ...(keepalive ? { persistentKeepalive: keepalive } : {}) }] } } }
      return computeStatus(buildView(map, status, off, now), 'wg0')
    }
    expect(tunnel(25, now / 1000 - 600)).toEqual({ tone: 'danger', label: 'peersOffline', note: { key: 'network.status.notes.lastHandshake', params: { handshakeAge: 600 } } })
    expect(tunnel(undefined, now / 1000 - 600)).toEqual({ tone: 'warning', label: 'idle', note: { key: 'network.status.notes.idleHandshake', params: { handshakeAge: 600 } } })
    expect(tunnel(25, null)).toEqual({ tone: 'danger', label: 'peersOffline', note: { key: 'network.status.notes.neverHandshake' } })
    expect(tunnel(undefined, null)).toEqual({ tone: 'warning', label: 'idle', note: { key: 'network.status.notes.idleNever' } })
  })

  it('says why the access point is down and leaves a missing interface unobserved', () => {
    const ap: WifiView = { radio: 'wlan0', mode: 'ap', apRunning: false, apReason: 'radio_blocked', stations: 0 }
    const status = observed()
    status.interfaces.entries!.push({ name: 'eth9', present: false } as never)
    const joined = buildView({ ...configured() as NetworkMap, eth9: { dhcp: true } }, status, ap)
    expect(computeStatus(joined, 'wlan0')).toEqual({ tone: 'danger', label: 'apDown', note: { key: 'network.status.notes.apReason.radio_blocked' } })
    expect(computeStatus(joined, 'eth9').label).toBe('notDetected')
  })

  it('warns while the access point runs on the fallback channel', () => {
    const ap: WifiView = { radio: 'wlan0', mode: 'ap', apRunning: true, apReason: 'acs_failed', apSsid: 'mica-789Z', stations: 0 }
    const joined = buildView(configured() as NetworkMap, observed(), ap)
    expect(computeStatus(joined, 'wlan0')).toEqual({ tone: 'warning', label: 'apRunning', note: { key: 'network.status.notes.apFallback', params: { ssid: 'mica-789Z' } } })

    // Only that reason reaches a running access point's view.
    const ap0 = { mode: 'always', interface: 'wlan0', ssid: 'mica-789Z', channel: 0 } as never
    const at = (reason: string) => ({ ...observed(), accessPoint: { available: true, running: true, reason } })
    expect(wifiView(undefined, ap0, at('acs_failed')).apReason).toBe('acs_failed')
    expect(wifiView(undefined, ap0, at('driver_error')).apReason).toBeUndefined()
    // A device that reports no access point at all.
    expect(wifiView(undefined, ap0, { ...observed(), accessPoint: undefined } as never).apReason).toBeUndefined()
  })

  it('takes the session\'s interface from the device when it says', () => {
    expect(sessionInterface({ ...observed(), session: { interface: 'eth0', localAddress: '192.168.1.24' } }, '192.168.10.1')).toBe('eth0')
    expect(sessionInterface({ ...observed(), session: { interface: null } }, '192.168.10.1')).toBeNull()
  })

  it('names the interface that holds the address the browser came in on', () => {
    expect(sessionInterface(observed(), '192.168.10.1')).toBe('eth1')
    expect(sessionInterface(observed(), '203.0.113.9')).toBeNull()
    expect(sessionInterface(observed(), undefined)).toBeNull()
  })
})

describe('the review\'s lines', () => {
  it('lists only what changed between two configurations', () => {
    const rows = diffRows({ dhcp: true }, { dhcp: false, static: { address: '192.168.20.1/24' } }, 'physical', t)
    expect(rows.map(([label]) => label)).toEqual(['network.fields.method', 'network.fields.address', 'network.fields.gateway', 'network.fields.dns'])
  })

  it('lists a list field by the entries added and removed', () => {
    const route = (destination: string) => ({ destination, gateway: '192.168.10.2' })
    const before = { dhcp: false, static: { address: '192.168.10.1/24', dns: ['1.1.1.1', '9.9.9.9'] }, routes: [route('10.1.0.0/16'), route('10.2.0.0/16')] }
    const after = { dhcp: false, static: { address: '192.168.10.1/24', dns: ['9.9.9.9'] }, routes: [route('10.2.0.0/16'), route('10.3.0.0/16')] }
    expect(diffRows(before, after, 'physical', t).map(([label, , , items]) => [label, items])).toEqual([
      ['network.fields.dns', { added: [], removed: ['1.1.1.1'] }],
      ['network.fields.routes', { added: ['10.3.0.0/16 → 192.168.10.2'], removed: ['10.1.0.0/16 → 192.168.10.2'] }],
    ])
  })
})

describe('what the console refuses to stage', () => {
  const map = configured() as NetworkMap

  it('catches an overlapping address and a bad route', () => {
    const entry = { dhcp: false, static: { address: '192.168.1.5/24' }, routes: [{ destination: 'nowhere' }] }
    const problems = validateEntry('eth2', 'physical', entry, map, addressBook(map, { eth0: '192.168.1.24/24' }), false)
    expect(problems.address).toEqual({ key: 'network.problems.overlap', params: { name: 'eth0', address: '192.168.1.24/24' } })
    expect(problems.routes?.key).toBe('network.problems.route')
  })

  it('catches a VLAN ID taken on the same parent and a tunnel port taken by another tunnel', () => {
    const staged: NetworkMap = { ...map, 'eth0-10': { kind: 'vlan', dhcp: true, vlan: { parent: 'eth0', id: 10 } } }
    expect(validateEntry('lan', 'vlan', { kind: 'vlan', dhcp: true, vlan: { parent: 'eth0', id: 10 } }, staged, [], false).vlanId?.key).toBe('network.problems.vlanTaken')
    expect(validateEntry('wg1', 'wireguard', { kind: 'wireguard', static: { address: '10.9.0.1/24' }, wireguard: { listenPort: 51820, peers: [] } }, staged, [], false).listenPort?.key).toBe('network.problems.portTaken')
  })

  it('refuses a gateway and a default route on an interface off the internet', () => {
    const entry = { dhcp: false, internet: false, static: { address: '192.168.10.1/24', gateway: '192.168.10.254' }, routes: [{ destination: '0.0.0.0/0', gateway: '192.168.10.254' }] }
    const problems = validateEntry('eth1', 'physical', entry, map, [], false)
    expect(problems.gateway?.key).toBe('network.problems.gatewayOffInternet')
    expect(problems.routes?.key).toBe('network.problems.defaultRouteOffInternet')
    expect(validateEntry('eth1', 'physical', { ...entry, internet: true }, map, [], false)).toEqual({})
  })

  it('asks nothing of a bridge port\'s addressing', () => {
    expect(validateEntry('eth1', 'physical', { dhcp: false }, map, [], true)).toEqual({})
  })
})

describe('the route table', () => {
  it('lists the main table\'s IPv4 routes of listed interfaces, default first, then by interface and prefix', () => {
    const view = buildView(configured() as NetworkMap, observed(), off)
    expect(routeTable(view).map((row) => [row.destination, row.via ?? null, row.interface, row.source, row.role ?? null])).toEqual([
      ['0.0.0.0/0', '192.168.1.1', 'eth0', 'dhcp', null],
      ['192.168.1.1/32', null, 'eth0', 'dhcp', null],
      ['192.168.1.0/24', null, 'eth0', 'link', null],
      ['192.168.10.0/24', null, 'eth1', 'link', null],
      ['10.20.0.0/16', '192.168.10.2', 'eth1', 'static', null],
      ['10.8.0.0/24', null, 'wg0', 'wireguard', null],
    ])
  })

  it('marks the default route the kernel uses and the others as standby', () => {
    const status = observed()
    const wlan = status.interfaces.entries!.find((entry) => entry.name === 'wlan0')!
    wlan.routes = [{ dest: '0.0.0.0/0', via: '192.168.1.1', metric: 600, source: 'DHCPv4', protocol: 'dhcp', table: 'main', family: 'ipv4' }]
    status.defaultRoutes.entries = [
      { family: 'ipv4', gateway: '192.168.1.1', interface: 'wlan0', metric: 600, effective: false },
      { family: 'ipv4', gateway: '192.168.1.1', interface: 'eth0', metric: 1024, effective: true },
    ]
    const view = buildView(configured() as NetworkMap, status, off)
    expect(exitOf(view)).toBe('eth0')
    expect(view.routes.map((route) => route.interface)).toEqual(['eth0', 'wlan0'])
    const defaults = routeTable(view).filter((row) => row.destination === '0.0.0.0/0')
    expect(defaults.map((row) => [row.interface, row.role])).toEqual([['eth0', 'effective'], ['wlan0', 'standby']])
  })

  it('leaves out routes of hidden links and other tables', () => {
    const status = observed()
    status.interfaces.entries!.push({ name: 'dummy0', index: 9, kind: 'dummy', link: { carrier: true }, addresses: [], dhcp: { available: true }, dns: [], routes: [{ dest: '10.99.0.0/24', table: 'main', family: 'ipv4' }] })
    status.interfaces.entries!.find((entry) => entry.name === 'eth1')!.routes!.push({ dest: '10.30.0.0/16', via: '192.168.10.3', table: 'vpn', family: 'ipv4' })
    const view = buildView(configured() as NetworkMap, status, off)
    expect(routeTable(view).some((row) => row.destination === '10.99.0.0/24' || row.destination === '10.30.0.0/16')).toBe(false)
  })
})

describe('a new tunnel\'s defaults', () => {
  it('take the first name, subnet and port nothing on the device has', () => {
    const view = buildView(configured() as NetworkMap, observed(), off)
    expect(tunnelDefaults(view, [])).toEqual({ name: 'wg1', address: '10.2.0.1/24', listenPort: 51821 })
    expect(tunnelDefaults(view, [51821, 51822]).listenPort).toBe(51823)
  })

  it('move past staged tunnels, peers\' subnets and observed routes', () => {
    const map = configured() as NetworkMap
    map.wg1 = { kind: 'wireguard', dhcp: false, static: { address: '10.2.0.1/24' }, wireguard: { listenPort: 51821, peers: [{ publicKey: 'k', allowedIps: ['10.3.0.0/24'] }] } }
    const status = observed()
    status.interfaces.entries!.find((entry) => entry.name === 'eth1')!.routes!.push({ dest: '10.4.0.0/16', via: '192.168.10.2', table: 'main', family: 'ipv4' })
    expect(tunnelDefaults(buildView(map, status, off), [])).toEqual({ name: 'wg2', address: '10.5.0.1/24', listenPort: 51822 })
  })

  it('reuse what a deleted tunnel held, applied or staged', () => {
    const map = configured() as NetworkMap
    delete map.wg0
    expect(tunnelDefaults(buildView(map, observed(), off), [])).toEqual({ name: 'wg0', address: '10.2.0.1/24', listenPort: 51821 })
    const status = observed()
    status.interfaces.entries = status.interfaces.entries!.filter((entry) => entry.name !== 'wg0')
    expect(tunnelDefaults(buildView(map, status, off), [])).toEqual({ name: 'wg0', address: '10.2.0.1/24', listenPort: 51821 })
  })
})

describe('the way out', () => {
  const client: WifiView = { ...off, mode: 'client' }

  it('is kept while one Ethernet port, bridge, VLAN or radio client keeps the internet', () => {
    const map = configured() as NetworkMap
    expect(keepsInternet(buildView(map, observed(), off))).toBe(true)
    const lan = { ...map, eth0: { dhcp: true, internet: false }, eth1: { ...(map.eth1 as object), internet: false }, eth2: { dhcp: true, internet: false } } as NetworkMap
    expect(keepsInternet(buildView(lan, observed(), off))).toBe(false)
    expect(keepsInternet(buildView(lan, observed(), client))).toBe(true)
    expect(keepsInternet(buildView({ ...lan, br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth0'] } } }, observed(), off))).toBe(true)
  })
})

describe('a board that declares its ports', () => {
  const declared = () => {
    const status = observed()
    status.board = 'declared'
    for (const entry of status.interfaces.entries!) {
      if (entry.name === 'eth1') entry.declared = { kind: 'ethernet', label: 'LAN1', order: 0, role: 'internal', capabilities: {} }
      if (entry.name === 'eth0') entry.declared = { kind: 'ethernet', label: 'WAN', order: 1, role: 'uplink', capabilities: { speedMbps: 1000 } }
      if (entry.name === 'can0') entry.declared = { kind: 'can', label: 'CAN', order: 3, capabilities: { fd: true } }
    }
    status.interfaces.entries!.push({ name: 'eth3', present: false, declared: { kind: 'ethernet', label: 'LAN2', order: 2, role: 'internal', capabilities: {} } } as never)
    return status
  }

  it('lists its ports first, in its order, present or not, and notes a port it does not declare', () => {
    const view = buildView(configured() as NetworkMap, declared(), off)
    expect(view.order.slice(0, 4)).toEqual(['eth1', 'eth0', 'eth3', 'can0'])
    expect(view.ifaces.eth3).toMatchObject({ kind: 'physical', declared: false, port: { label: 'LAN2' } })
    expect(computeStatus(view, 'eth3').label).toBe('notDetected')
    expect(undeclaredPort(view, 'eth2')).toBe(true)
    expect(undeclaredPort(view, 'eth0')).toBe(false)
    expect(undeclaredPort(buildView(configured() as NetworkMap, observed(), off), 'eth2')).toBe(false)
  })

  it('keeps an internal port off the internet until its entry says otherwise', () => {
    const map = { ...configured(), eth0: { dhcp: true, internet: false }, eth2: { dhcp: true, internet: false } } as NetworkMap
    const view = buildView(map, declared(), off)
    expect(internalPort(view, 'eth1')).toBe(true)
    expect(keepsInternet(view)).toBe(false)
    expect(keepsInternet(buildView({ ...map, eth1: { ...(map.eth1 as object), internet: true } } as NetworkMap, declared(), off))).toBe(true)
    const entry = { dhcp: false, static: { address: '192.168.10.1/24', gateway: '192.168.10.254' } }
    expect(validateEntry('eth1', 'physical', entry, map, [], false, true).gateway?.key).toBe('network.problems.gatewayOffInternet')
    expect(validateEntry('eth1', 'physical', { ...entry, internet: true }, map, [], false, true)).toEqual({})
  })
})
