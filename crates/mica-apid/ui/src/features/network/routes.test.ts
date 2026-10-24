import { describe, expect, it } from 'vitest'
import { configured, observed } from '@/shared/testing/network'
import type { ChangeSet, NetworkMap } from './model'
import { draftOf, draftProblems, routeTargets, stageRoute, staticRoutes, unstageRoute, type RouteDraft } from './routes'
import { buildView, type WifiView } from './view'

const off: WifiView = { radio: 'wlan0', mode: 'off', apRunning: false, stations: 0 }
const map = (): NetworkMap => ({
  ...configured() as NetworkMap,
  eth1: { dhcp: false, static: { address: '192.168.10.1/24' }, routes: [{ destination: '10.20.0.0/16', gateway: '192.168.10.2' }] },
  br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth2'] } },
})
const start = (): ChangeSet => ({ base: map(), staged: map() })
const view = (set: ChangeSet) => buildView(set.staged, observed(), off)
const draft = (fields: Partial<RouteDraft> = {}): RouteDraft => ({ iface: 'eth0', destination: '10.30.0.0/16', gateway: '', metric: '0', ...fields })

describe('the static routes', () => {
  it('lists every interface\'s routes in interface order and marks the ones not stored', () => {
    let set = start()
    set = stageRoute(set, view(set), draft({ gateway: '192.168.1.2', metric: '5' }))
    expect(staticRoutes(view(set), set.base).map((row) => [row.iface, row.route, row.pending])).toEqual([
      ['eth0', { destination: '10.30.0.0/16', gateway: '192.168.1.2', metric: 5 }, true],
      ['eth1', { destination: '10.20.0.0/16', gateway: '192.168.10.2' }, false],
    ])
  })

  it('offers the interfaces that carry addresses, physical ones alone on OpenRC', () => {
    expect(routeTargets(view(start()), false)).toEqual(['eth0', 'eth1', 'br0', 'wlan0', 'wg0'])
    expect(routeTargets(view(start()), true)).toEqual(['eth0', 'eth1', 'wlan0'])
  })

  it('stages a route on an interface the map does not declare with that interface\'s default', () => {
    const set = stageRoute(start(), view(start()), draft({ iface: 'wlan0' }))
    expect(set.staged.wlan0).toEqual({ dhcp: true, routes: [{ destination: '10.30.0.0/16' }] })
  })

  it('moves an edited route to the interface it now names and deletes one', () => {
    let set = start()
    const row = staticRoutes(view(set), set.base)[0]
    expect(draftOf(row)).toEqual({ iface: 'eth1', destination: '10.20.0.0/16', gateway: '192.168.10.2', metric: '0' })
    set = stageRoute(set, view(set), { ...draftOf(row), iface: 'eth0', gateway: '' }, row)
    expect(set.staged.eth1.routes).toBeUndefined()
    expect(set.staged.eth0.routes).toEqual([{ destination: '10.20.0.0/16' }])
    set = unstageRoute(set, view(set), staticRoutes(view(set), set.base)[0])
    expect(set.staged.eth0).toEqual({ dhcp: true })
  })

  it('refuses a bad destination, gateway or metric, the default route and a duplicate, by field', () => {
    const at = view(start())
    expect(draftProblems(draft({ destination: 'nowhere' }), at).destination?.key).toBe('network.problems.cidr')
    expect(draftProblems(draft({ destination: '0.0.0.0/0' }), at).destination?.key).toBe('network.routes.problems.noDefault')
    expect(draftProblems(draft({ gateway: '192.168.1' }), at).gateway?.key).toBe('network.problems.ipv4')
    expect(draftProblems(draft({ metric: '-1' }), at).metric?.key).toBe('network.routes.problems.metric')
    expect(draftProblems(draft({ iface: '' }), at).iface?.key).toBe('network.routes.problems.iface')
    expect(draftProblems(draft({ iface: 'eth1', destination: '10.20.0.0/16' }), at).destination?.key).toBe('network.routes.problems.duplicate')
    const row = staticRoutes(at, start().base)[0]
    expect(draftProblems(draftOf(row), at, row)).toEqual({})
    expect(draftProblems(draft(), at)).toEqual({})
  })
})
