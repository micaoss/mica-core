import { describe, expect, it } from 'vitest'
import { DEFAULT_TARGETS, megabytesPerDay, probeProblem, switchProbe, uplinkOf } from './uplink'
import { observed } from '@/shared/testing/network'

describe('the reachability check', () => {
  it('stores the two default targets when switched on with none, and keeps them when switched off', () => {
    const on = switchProbe({ dhcp: true }, true)
    expect(on.probe).toEqual({ enabled: true, targets: [{ address: '1.1.1.1', method: 'tcp', port: 53 }, { address: '8.8.8.8', method: 'tcp', port: 53 }] })
    expect(on.probe?.targets).not.toBe(DEFAULT_TARGETS)
    const off = switchProbe(on, false)
    expect(off.probe).toEqual({ ...on.probe, enabled: false })
    const own = switchProbe({ dhcp: true, probe: { enabled: false, targets: [{ address: 'gateway', method: 'icmp' }] } }, true)
    expect(own.probe?.targets).toEqual([{ address: 'gateway', method: 'icmp' }])
  })

  it('refuses what the device would refuse', () => {
    const target = { address: '1.1.1.1', method: 'tcp' as const, port: 53 }
    expect(probeProblem({ enabled: true, targets: [target] })).toBeUndefined()
    expect(probeProblem({ enabled: true, targets: [] })).toBe('network.uplink.problems.targets')
    expect(probeProblem({ enabled: true, targets: [target, target] })).toBe('network.uplink.problems.targets')
    expect(probeProblem({ enabled: true, targets: [{ ...target, address: 'example.org' }] })).toBe('network.uplink.problems.address')
    expect(probeProblem({ enabled: true, targets: [{ ...target, port: 0 }] })).toBe('network.uplink.problems.port')
    expect(probeProblem({ enabled: true, targets: [target], intervalS: 1 })).toBe('network.uplink.problems.interval')
    expect(probeProblem({ enabled: true, targets: [target], intervalS: 2, timeoutMs: 2000 })).toBe('network.uplink.problems.timeout')
    expect(probeProblem({ enabled: true, targets: [target], successesToUp: 51 })).toBe('network.uplink.problems.threshold')
  })

  it('costs about 1.6 MB a day per target at the default interval', () => {
    expect(megabytesPerDay(10)).toBe(1.6)
    expect(megabytesPerDay(60)).toBe(0.3)
  })

  it('reads one uplink of the observation', () => {
    const status = observed({ uplinks: { active: 'eth1', natural: 'eth0', override: true, lastChange: null, uplinks: [{ name: 'eth0', metric: 100, state: 'down', probe: null }] } })
    expect(uplinkOf(status, 'eth0')).toEqual({ uplink: { name: 'eth0', metric: 100, state: 'down', probe: null }, active: false, failover: true })
    expect(uplinkOf(status, 'eth1').active).toBe(true)
    expect(uplinkOf(undefined, 'eth0').uplink).toBeUndefined()
  })
})
