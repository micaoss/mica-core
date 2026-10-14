import { describe, expect, it } from 'vitest'
import { configuredSummary, networkRows, physicalInterfaces, selectUplink, visibleNetworkRows, listedAddresses } from './network'
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

describe('network rows', () => {
  it('keeps observed-only and configured-but-missing interfaces visible', () => {
    const rows = networkRows(
      { eth0: { kind: 'physical', dhcp: true }, missing0: { kind: 'vlan' } },
      [
        { index: 1, name: 'lo', operationalState: 'carrier' },
        { index: 2, name: 'eth0', operationalState: 'routable' },
      ],
    )

    expect(rows.map((row) => row.name)).toEqual(['lo', 'eth0', 'missing0'])
    expect(rows[0].configured).toBeUndefined()
    expect(rows[1].configured).toEqual({ kind: 'physical', dhcp: true })
    expect(rows[2].observed).toBeUndefined()
  })

  it('describes the configured kind and addressing method separately', () => {
    expect(configuredSummary({ kind: 'bridge', static: { address: '10.0.0.2/24' } }))
      .toBe('bridge · Static')
    expect(configuredSummary()).toBe('Not configured')
  })
})

describe('the visible interfaces', () => {
  const rows = networkRows(
    { eth0: { kind: 'physical', dhcp: true }, br0: { kind: 'bridge', bridge: { ports: ['eth1'] } } },
    [
      { index: 1, name: 'lo', type: 'loopback' },
      { index: 8, name: 'lo0', type: 'loopback' },
      { index: 9, name: 'dummy0', kind: 'dummy' },
      // Theirs, not the kernel's: a name that merely starts with `lo`.
      { index: 10, name: 'lolink0' },
      { index: 2, name: 'eth0' },
      { index: 3, name: 'eth1' },
      { index: 4, name: 'wlan0' },
      { index: 5, name: 'veth1a2b3c', kind: 'veth' },
      { index: 6, name: 'podman0', kind: 'bridge' },
      { index: 7, name: 'br0', kind: 'bridge' },
    ],
  )

  it('hides the loopback and the virtual ends the container engine makes, keeping what was declared', () => {
    expect(visibleNetworkRows(rows).map((row) => row.name)).toEqual(['eth0', 'eth1', 'wlan0', 'br0', 'lolink0'])
  })

  it('offers only kernel links as a VLAN parent or a bridge port', () => {
    expect(physicalInterfaces(rows)).toEqual(['eth0', 'eth1', 'wlan0', 'lolink0'])
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

describe('the addresses an interface lists', () => {
  /// networkd reports an address as its bytes. Joined with dots, an IPv6
  /// address is sixteen numbers nobody can type.
  it('spells IPv4 and IPv6 from networkd\'s bytes and leaves link-local out', () => {
    expect(listedAddresses([
      { Address: [192, 168, 1, 24], PrefixLength: 24 },
      { Address: [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0x11, 0x22, 0xff, 0xfe, 0x33, 0x44, 0x55], PrefixLength: 64 },
      { Address: [0x24, 0x08, 0x82, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01], PrefixLength: 64 },
      { Address: [169, 254, 3, 4], PrefixLength: 16 },
    ])).toBe('192.168.1.24/24, 2408:8207::1/64')
  })

  it('takes addresses already spelled, and says so when there is none to list', () => {
    expect(listedAddresses(['10.0.0.2/24', 'fe80::1/64'])).toBe('10.0.0.2/24')
    expect(listedAddresses([{ address: 'fd00::2', prefixLength: 64 }])).toBe('fd00::2/64')
    expect(listedAddresses([{ Address: [127, 0, 0, 1], PrefixLength: 8 }])).toBe('—')
    expect(listedAddresses(undefined)).toBe('—')
  })
})
