import { describe, expect, it } from 'vitest'
import { isHiddenInterface, selectUplink } from './network'
import type { ObservedNetworkInterface, ObservedNetworkState } from './types'

function observed(
  entries: Partial<ObservedNetworkInterface>[],
  defaultRoutes: { gateway?: string; interface?: string }[] = [],
): ObservedNetworkState {
  return {
    interfaces: {
      available: true,
      count: entries.length,
      entries: entries.map((entry) => ({
        name: 'unnamed',
        link: {},
        addresses: [],
        dhcp: { available: false },
        dns: [],
        ...entry,
      })),
    },
    defaultRoutes: { available: true, count: defaultRoutes.length, entries: defaultRoutes },
    accessPoint: { available: false },
    dns: { available: false },
    wifi: { available: false },
    capabilities: {
      wifi: { supported: false, interfaces: [] },
      bluetooth: { supported: false, adapters: [] },
      cellular: { supported: false, interfaces: [] },
    },
  }
}

const loopback: Partial<ObservedNetworkInterface> = {
  name: 'lo',
  index: 1,
  type: 'loopback',
  addresses: [{ family: 'ipv4', address: '127.0.0.1', prefixLength: 8, scope: 'host' }],
}

describe('the hidden interfaces', () => {
  it('hides the loopback and the virtual ends the container engine makes', () => {
    const hidden = [['lo', 'loopback'], ['lo0', undefined], ['dummy0', 'dummy'], ['veth1a2b3c', 'veth'], ['podman0', 'bridge'], ['docker0', undefined]] as const
    for (const [name, kind] of hidden) expect(isHiddenInterface(name, kind)).toBe(true)
  })

  it('keeps the links an operator came for, and a name that merely starts with `lo`', () => {
    for (const name of ['eth0', 'wlan0', 'br0', 'lolink0']) expect(isHiddenInterface(name, undefined)).toBe(false)
  })
})

describe('the uplink', () => {
  it('is the interface a default route leaves by, not the first addressed one', () => {
    const uplink = selectUplink(observed(
      [
        loopback,
        {
          name: 'wg0',
          index: 3,
          kind: 'wireguard',
          addresses: [{ family: 'ipv4', address: '10.7.0.2', prefixLength: 32, scope: 'global' }],
        },
        {
          name: 'eth0',
          index: 2,
          link: { operationalState: 'routable' },
          addresses: [
            { family: 'ipv6', address: 'fd00::24', prefixLength: 64, scope: 'global' },
            { family: 'ipv4', address: '192.168.1.24', prefixLength: 24, scope: 'global' },
          ],
        },
      ],
      [{ gateway: '192.168.1.1', interface: 'eth0' }],
    ))

    expect(uplink?.name).toBe('eth0')
    expect(uplink?.address).toBe('192.168.1.24/24')
    expect(uplink?.operationalState).toBe('routable')
  })

  it('falls back to the first globally addressed interface when no default route is observed', () => {
    const uplink = selectUplink(observed([
      loopback,
      {
        name: 'eth0',
        index: 2,
        addresses: [
          { family: 'ipv4', address: '169.254.7.1', prefixLength: 16, scope: 'link' },
          { family: 'ipv4', address: '10.0.0.5', prefixLength: 24, scope: 'global' },
        ],
      },
    ]))

    expect(uplink?.name).toBe('eth0')
    expect(uplink?.address).toBe('10.0.0.5/24')
  })

  it('reports nothing rather than the loopback when nothing else is addressed', () => {
    expect(selectUplink(observed([loopback]))).toBeUndefined()
    expect(selectUplink(undefined)).toBeUndefined()
  })

  it('keeps an interface a default route names even when its address is not observed', () => {
    const uplink = selectUplink(observed(
      [loopback, { name: 'wwan0', index: 4 }],
      [{ gateway: '10.64.64.64', interface: 'wwan0' }],
    ))

    expect(uplink?.name).toBe('wwan0')
    expect(uplink?.address).toBeUndefined()
  })
})
