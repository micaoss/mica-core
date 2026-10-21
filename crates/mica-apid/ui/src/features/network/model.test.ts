import { describe, expect, it } from 'vitest'
import {
  defaultPool,
  discardAll,
  inSubnet,
  nextName,
  overlaps,
  pendingChanges,
  poolFromRange,
  poolRange,
  rebase,
  sameEntry,
  stageCreate,
  stageRemove,
  stageRevert,
  stageSet,
  touchesSession,
  withFreshPeers,
  type ChangeSet,
  type NetworkMap,
} from './model'

const base: NetworkMap = {
  eth0: { dhcp: true },
  eth1: { dhcp: false, static: { address: '192.168.10.1/24' } },
}
const start = (map: NetworkMap = base): ChangeSet => ({ base: structuredClone(map), staged: structuredClone(map) })
const kindOf = () => 'physical' as const

describe('the change set', () => {
  it('stages nothing when an edit lands where it started', () => {
    const set = start()
    expect(stageSet(set, 'eth0', 'physical', (entry) => ({ ...entry }))).toBe(set)
    expect(stageSet(set, 'eth2', 'physical', () => ({ dhcp: true }))).toBe(set)
  })

  it('takes a joining port\'s addressing away and gives a leaving port DHCP back', () => {
    let set = stageCreate(start(), { kind: 'bridge', name: 'br0' })
    set = stageSet(set, 'br0', 'bridge', (entry) => ({ ...entry, bridge: { ports: ['eth1'] } }))
    expect(set.staged.eth1).toEqual({ dhcp: false })
    set = stageSet(set, 'br0', 'bridge', (entry) => ({ ...entry, bridge: { ports: [] } }))
    expect(set.staged.eth1).toEqual({ dhcp: true })
  })

  it('declares a VLAN\'s parent when it was not', () => {
    const set = stageCreate(start({}), { kind: 'vlan', name: 'eth0-10', parent: 'eth0', vlanId: 10 })
    expect(set.staged.eth0).toEqual({ dhcp: true })
    expect(set.staged['eth0-10']).toEqual({ kind: 'vlan', dhcp: true, vlan: { parent: 'eth0', id: 10 } })
  })

  it('lists creations, deletions and updates in the given order', () => {
    let set = stageCreate(start(), { kind: 'wireguard', name: 'wg0', address: '10.8.0.1/24', listenPort: 51820 })
    set = stageSet(set, 'eth1', 'physical', (entry) => ({ ...entry, static: { address: '192.168.20.1/24' } }))
    set = stageRemove(set, 'eth0', 'physical')
    expect(pendingChanges(set, kindOf, ['eth0', 'eth1', 'wg0'])).toEqual([
      { name: 'eth0', op: 'update', kind: 'physical' },
      { name: 'eth1', op: 'update', kind: 'physical' },
      { name: 'wg0', op: 'create', kind: 'wireguard' },
    ])
  })

  it('reverts one entry and discards all of them', () => {
    let set = stageSet(start(), 'eth0', 'physical', () => ({ dhcp: false }))
    set = stageCreate(set, { kind: 'bridge', name: 'br0' })
    expect(stageRevert(set, 'eth0').staged.eth0).toEqual({ dhcp: true })
    expect(stageRevert(set, 'br0').staged.br0).toBeUndefined()
    expect(pendingChanges(discardAll(set), kindOf)).toEqual([])
  })

  it('compares entries by meaning, not by spelling', () => {
    expect(sameEntry({ kind: 'physical', dhcp: true, dns: [] }, { dhcp: true })).toBe(true)
    expect(sameEntry({ dhcp: true }, { dhcp: false })).toBe(false)
  })
})

describe('the session guard', () => {
  it('asks when the session interface, its bridge or its parent changes, and always when the session is unknown', () => {
    const map: NetworkMap = { ...base, br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth0'] } } }
    const set = stageSet(start(map), 'br0', 'bridge', (entry) => ({ ...entry, dhcp: false, static: { address: '10.0.0.1/24' } }))
    const changes = pendingChanges(set, kindOf)
    expect(touchesSession(changes, set, 'eth0')).toBe(true)
    expect(touchesSession(changes, set, 'eth1')).toBe(false)
    expect(touchesSession(changes, set, null)).toBe(true)
    expect(touchesSession([], set, null)).toBe(false)
  })
})

describe('a fresh read under a change set', () => {
  it('follows the device where nothing was staged', () => {
    const set = stageSet(start(), 'eth0', 'physical', () => ({ dhcp: false }))
    const fresh = { ...base, eth1: { dhcp: true } }
    const result = rebase(set, fresh)
    expect(result.conflicts).toEqual([])
    expect(result.set.staged.eth1).toEqual({ dhcp: true })
    expect(result.set.staged.eth0).toEqual({ dhcp: false })
  })

  it('reports an entry that moved underneath a staged edit', () => {
    const set = stageSet(start(), 'eth0', 'physical', () => ({ dhcp: false }))
    const result = rebase(set, { ...base, eth0: { dhcp: true, dns: ['1.1.1.1'] } })
    expect(result.conflicts).toEqual(['eth0'])
    expect(result.set).toBe(set)
  })

  it('takes peers added since staging into the staged tunnel', () => {
    const map: NetworkMap = { wg0: { kind: 'wireguard', static: { address: '10.8.0.1/24' }, wireguard: { listenPort: 51820, peers: [] } } }
    const set = stageSet(start(map), 'wg0', 'wireguard', (entry) => ({ ...entry, wireguard: { ...entry.wireguard!, listenPort: 51821 } }))
    const peer = { publicKey: 'k', allowedIps: ['10.8.0.2/32'] }
    const fresh: NetworkMap = { wg0: { ...map.wg0, wireguard: { listenPort: 51820, peers: [peer] } } }
    const result = rebase(set, fresh)
    expect(result.conflicts).toEqual([])
    expect(result.set.staged.wg0.wireguard).toEqual({ listenPort: 51821, peers: [peer] })
    expect(withFreshPeers(set.staged, fresh).wg0.wireguard?.peers).toEqual([peer])
  })
})

describe('IPv4 and the DHCP pool', () => {
  it('tells host addresses and overlapping subnets', () => {
    expect(inSubnet('192.168.10.1', '192.168.10.0/24')).toBe(true)
    expect(inSubnet('192.168.10.255', '192.168.10.0/24')).toBe(false)
    expect(overlaps('10.0.0.0/8', '10.20.0.1/16')).toBe(true)
    expect(overlaps('192.168.1.1/24', '192.168.2.1/24')).toBe(false)
  })

  it('converts a pool between start–end and offset/size', () => {
    expect(poolRange('192.168.10.1/24', { poolOffset: 100, poolSize: 50 })).toEqual({ start: '192.168.10.100', end: '192.168.10.149' })
    expect(poolFromRange('192.168.10.1/24', '192.168.10.100', '192.168.10.149')).toEqual({ poolOffset: 100, poolSize: 50 })
    expect(poolFromRange('192.168.10.1/24', '192.168.11.1', '192.168.10.149')).toBeUndefined()
  })

  it('offers a default pool only where the subnet can serve one', () => {
    expect(defaultPool('192.168.10.1/24')).toEqual({ poolOffset: 100, poolSize: 50 })
    expect(defaultPool('10.0.0.1/30')).toBeUndefined()
  })

  it('names the next free interface', () => {
    expect(nextName((name) => ['br1', 'br2'].includes(name), 'br', 1)).toBe('br3')
  })
})
