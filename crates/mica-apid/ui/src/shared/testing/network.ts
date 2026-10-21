import type { ObservedNetworkInterface, ObservedNetworkState } from '@/lib/types'

/// A bench device for the network pages: an uplink on eth0, a LAN with a DHCP
/// server on eth1, an undeclared eth2 with no cable, a radio, a tunnel with
/// one peer and a CAN controller.

export const PEER_KEY = 'xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg='

export function configured() {
  return {
    eth0: { dhcp: true },
    eth1: { dhcp: false, static: { address: '192.168.10.1/24' } },
    wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.8.0.1/24' }, wireguard: { listenPort: 51820, peers: [{ publicKey: PEER_KEY, allowedIps: ['10.8.0.2/32'], persistentKeepalive: 25 }] } },
    can0: { kind: 'can', can: { bitrate: 250000 } },
  } as Record<string, unknown>
}

function link(name: string, index: number, fields: Partial<ObservedNetworkInterface> = {}): ObservedNetworkInterface {
  return { name, index, link: { carrier: true, operationalState: 'routable' }, addresses: [], dhcp: { available: true }, dns: [], ...fields }
}

const v4 = (address: string, prefixLength: number, configSource = 'static') => ({ family: 'ipv4', address, prefixLength, scope: 'global', configSource })

export function observed(overrides: Partial<ObservedNetworkState> = {}): ObservedNetworkState {
  const entries = [
    link('lo', 1, { type: 'loopback', addresses: [{ family: 'ipv4', address: '127.0.0.1', prefixLength: 8, scope: 'host' }] }),
    link('eth0', 2, { hardwareAddress: '02:00:00:00:00:01', addresses: [v4('192.168.1.24', 24, 'DHCPv4')], dns: ['192.168.1.1'], dhcp: { available: true, lease: { server: '192.168.1.1', router: '192.168.1.1', lifetimeSeconds: 3600 } } }),
    link('eth1', 3, { hardwareAddress: '02:00:00:00:00:02', addresses: [v4('192.168.10.1', 24)] }),
    link('eth2', 4, { hardwareAddress: '02:00:00:00:00:03', link: { carrier: false, operationalState: 'no-carrier' } }),
    link('wlan0', 5, { type: 'wlan', link: { carrier: false, operationalState: 'no-carrier' } }),
    link('wg0', 6, { kind: 'wireguard', addresses: [v4('10.8.0.1', 24)] }),
    link('can0', 7, { type: 'can' }),
  ]
  return {
    interfaces: { available: true, count: entries.length, entries },
    defaultRoutes: { available: true, count: 1, entries: [{ family: 'ipv4', gateway: '192.168.1.1', interface: 'eth0', metric: 1024 }] },
    dns: { available: true, linkServers: ['192.168.1.1'], resolverServers: ['192.168.1.1'], probe: { name: 'example.org', reachable: true, result: 'ok' } },
    wifi: { available: true, associations: [] },
    accessPoint: { available: true, entries: [] },
    capabilities: {
      wifi: { supported: true, interfaces: ['wlan0'] },
      bluetooth: { supported: false, adapters: [] },
      cellular: { supported: false, interfaces: [] },
    },
    ...overrides,
  }
}

export function accessPoint(mode: 'off' | 'provisioning' | 'always' = 'off') {
  return { mode, interface: 'wlan0', ssid: 'mica-lab', psk: '<redacted>', channel: 6, countryCode: 'DE', address: '192.168.4.1/24', holdDownSeconds: 120, graceSeconds: 60 }
}

/// The reads every network page makes, answered for the bench device.
export function networkRoutes(map: Record<string, unknown> = configured(), status: ObservedNetworkState = observed()) {
  return {
    '/api/v1/meta': { api: 'v1', features: ['wifi', 'bluetooth'], init: 'systemd' },
    '/api/v1/network': { configured: map, configuredCount: Object.keys(map).length, version: 'v1', observed: { available: true, interfaceCount: 0, interfaces: [] } },
    '/api/v1/network/status': status,
    '/api/v1/settings/hostname': 'mica-bench',
    '/api/v1/dns': {
      global: { enabled: false, servers: ['9.9.9.9'] },
      mode: 'interface',
      effective: { links: [{ name: 'eth0', servers: ['192.168.1.1'], source: 'dhcp' }, { name: 'eth1', servers: [], source: 'static' }] },
      staticRecords: [{ id: 'r1', hostname: 'nas.lan', address: '192.168.1.20', comment: 'Storage' }],
      probe: { ok: true, name: 'example.org', latencyMs: 18, checkedAt: '2026-10-21T08:00:00Z' },
    },
    '/api/v1/wifi/client': { enabled: false, interface: 'wlan0' },
    '/api/v1/wifi/ap': accessPoint(),
    '/api/v1/wifi/client/networks': [{ ssid: 'marina', psk: '<redacted>', hidden: false, priority: 10 }],
    '/api/v1/state/network': { wg0: { publicKey: 'Q2hhbmdlTWVQbGVhc2VDaGFuZ2VNZVBsZWFzZTAwMDA=' } },
    '/api/v1/network/wg0/peers': [{ name: 'Harbour office', publicKey: PEER_KEY, presharedKey: true, allowedIps: ['10.8.0.2/32'], persistentKeepalive: 25, latestHandshake: null, rxBytes: null, txBytes: null }],
  } as Record<string, unknown>
}
