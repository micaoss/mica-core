import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { InterfaceDetailPage } from './interface-detail'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const ROUTE = '/network_/$name'

function render(name: string) {
  return renderParamRoute(<InterfaceDetailPage />, ROUTE, `/network_/${name}`)
}

const configured = {
  eth0: { kind: 'physical', dhcp: true },
  eth1: { kind: 'physical', dhcp: false, static: { address: '10.0.0.9/24', dns: [] } },
  'eth0.100': { kind: 'vlan', dhcp: true, vlan: { parent: 'eth0', id: 100 } },
  // Not a port of anything: routes and a server are offered on a link whose
  // addressing is its own.
  eth2: { kind: 'physical', dhcp: false, static: { address: '10.0.0.9/24', dns: [] } },
  br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth1'] } },
  wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.7.0.2/32', dns: [] }, wireguard: { listenPort: 51820, peers: [] } },
}

const routes = {
  '/api/v1/network': {
    configured,
    configuredCount: 5,
    observed: {
      available: true,
      interfaceCount: 3,
      interfaces: [
        { index: 1, name: 'lo', type: 'loopback' },
        { index: 2, name: 'eth0', type: 'ether' },
        { index: 3, name: 'eth1', type: 'ether' },
      ],
    },
  },
  '/api/v1/network/status': {
    interfaces: {
      available: true,
      count: 1,
      entries: [{
        name: 'eth0',
        type: 'ether',
        driver: 'rk_gmac-dwmac',
        mtu: 1500,
        link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
        hardwareAddress: 'aa:bb:cc:dd:ee:ff',
        addresses: [{ family: 'ipv4', address: '192.168.1.20', prefixLength: 24, scope: 'global', configSource: 'DHCPv4' }],
        dhcp: { available: true, state: 'bound', lease: { server: '192.168.1.1' } },
        dns: ['192.168.1.1'],
      }],
    },
    defaultRoutes: { available: true, count: 0, entries: [] },
    dns: { available: false },
    wifi: { available: false },
    capabilities: {
      wifi: { supported: false, interfaces: [] },
      bluetooth: { supported: false, adapters: [] },
      cellular: { supported: false, interfaces: [] },
    },
  },
  '/api/v1/health': { apid: 'ok', micad: 'ok', checkedAt: 1 },
}

describe('one interface', () => {
  it('shows what the device observes about this link and nothing about the others', async () => {
    stubFetch(routes)
    render('eth0')

    expect(await screen.findByText('192.168.1.20/24 · DHCPv4')).toBeTruthy()
    expect(screen.getByText('bound · server 192.168.1.1')).toBeTruthy()
    expect(screen.queryByText('eth1')).toBeNull()
  })

  it('says so rather than showing an empty reading when the link is not observed', async () => {
    stubFetch(routes)
    render('br0')

    expect(await screen.findAllByText('not observed')).not.toHaveLength(0)
  })

  /// The parent is a choice among the links the kernel has, not free text: an
  /// undeclared parent is a VLAN micad refuses and networkd would never build.
  it('edits a VLAN against the interfaces the device actually has', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/network/eth0.100': () => jsonResponse({ taskId: 'task-1' }, 202) })
    render('eth0.100')

    const parent = await screen.findByRole('combobox', { name: 'Parent interface' })
    await userEvent.click(parent)
    await userEvent.click(await screen.findByRole('option', { name: 'eth1' }))
    await userEvent.click(screen.getByRole('button', { name: 'Review and save' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Apply' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(true))
    const call = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
    expect(JSON.parse(String((call?.[1] as RequestInit).body)).vlan).toEqual({ parent: 'eth1', id: 100 })
  })

  it('takes bridge ports as a choice of links and writes the set', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/network/br0': () => jsonResponse({ taskId: 'task-2' }, 202) })
    render('br0')

    const ports = await screen.findByRole('group', { name: 'Bridge ports' })
    await userEvent.click(within(ports).getByRole('checkbox', { name: 'eth0' }))
    await userEvent.click(screen.getByRole('button', { name: 'Review and save' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Apply' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body)).bridge).toEqual({ ports: ['eth1', 'eth0'] })
  })

  /// A tunnel has no DHCP client and no gateway. Offering either would offer a
  /// configuration the device refuses.
  it('offers a tunnel its listen port and neither DHCP nor a gateway', async () => {
    stubFetch(routes)
    render('wg0')

    expect(await screen.findByLabelText('Listen port')).toBeTruthy()
    expect(screen.queryByLabelText('Gateway')).toBeNull()
    expect(screen.queryByRole('radio', { name: 'DHCP' })).toBeNull()
  })

  /// Routes and a server are edited with the addressing they depend on and
  /// written in the same save: three writes would leave a device addressed for
  /// a subnet it is not yet serving.
  it('writes static routes and a DHCP server with the addressing they belong to', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/network/eth2': () => jsonResponse({ taskId: 'task-3' }, 202) })
    render('eth2')

    // The fixture's `eth1` is a port of `br0`; a port's addressing is the
    // bridge's, so the fieldsets below are deliberately not offered there.
    await userEvent.click(await screen.findByRole('button', { name: 'Add route' }))
    await userEvent.type(screen.getByLabelText('Destination'), '10.20.0.0/16')
    await userEvent.type(screen.getByLabelText('Via'), '10.0.0.1')
    await userEvent.click(screen.getByRole('checkbox', { name: 'Offer a DHCP server on this interface' }))
    await userEvent.click(screen.getByRole('button', { name: 'Review and save' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Apply' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    const body = JSON.parse(String((call?.[1] as RequestInit).body))
    expect(body.routes).toEqual([{ destination: '10.20.0.0/16', gateway: '10.0.0.1' }])
    expect(body.dhcpServer).toEqual({ poolOffset: 100, poolSize: 50, dns: [] })
  })

  /// A link that takes its own address from a server cannot be one.
  it('offers no DHCP server on a link that is itself a DHCP client', async () => {
    stubFetch(routes)
    render('eth0')

    expect(await screen.findByRole('button', { name: 'Review and save' })).toBeTruthy()
    expect(screen.queryByRole('checkbox', { name: 'Offer a DHCP server on this interface' })).toBeNull()
  })

  it('deletes the entry behind a confirmation and returns to the list', async () => {
    const fetch = stubFetch({ ...routes, 'DELETE /api/v1/network/eth1': () => jsonResponse(null, 204) })
    render('eth1')

    await userEvent.click(await screen.findByRole('button', { name: 'Delete interface' }))
    const dialog = await screen.findByRole('alertdialog')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Delete interface' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input, init]) =>
      String(input) === '/api/v1/network/eth1' && (init as RequestInit | undefined)?.method === 'DELETE')).toBe(true))
  })
})
