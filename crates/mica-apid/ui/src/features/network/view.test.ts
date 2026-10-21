import { describe, expect, it } from 'vitest'
import { configured, observed } from '@/shared/testing/network'
import type { NetworkMap } from './model'
import { diffRows } from './summary'
import { addressBook, validateEntry } from './validate'
import { buildView, computeStatus, exitOf, sessionInterface, type WifiView } from './view'

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
    expect(computeStatus(joined, 'wg0')).toEqual({ tone: 'success', label: 'connected', note: { key: 'network.status.notes.peersUp', params: { up: 1, count: 2 } } })
  })

  it('says why the access point is down and leaves a missing interface unobserved', () => {
    const ap: WifiView = { radio: 'wlan0', mode: 'ap', apRunning: false, apReason: 'radio_blocked', stations: 0 }
    const status = observed()
    status.interfaces.entries!.push({ name: 'eth9', present: false } as never)
    const joined = buildView({ ...configured() as NetworkMap, eth9: { dhcp: true } }, status, ap)
    expect(computeStatus(joined, 'wlan0')).toEqual({ tone: 'danger', label: 'apDown', note: { key: 'network.status.notes.apReason.radio_blocked' } })
    expect(computeStatus(joined, 'eth9').label).toBe('notCreated')
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

  it('asks nothing of a bridge port\'s addressing', () => {
    expect(validateEntry('eth1', 'physical', { dhcp: false }, map, [], true)).toEqual({})
  })
})
