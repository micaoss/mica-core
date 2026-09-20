import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderRoute, stubFetch } from '@/shared/testing/panel'
import { NetworkPage } from './network-page'

const routes = {
  '/api/v1/network': {
    configured: { eth0: { dhcp: true }, wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.10.0.2/24', dns: [] }, wireguard: { listenPort: 51820, peers: [] } } },
    configuredCount: 2,
    observed: { available: true, interfaceCount: 2, interfaces: [{ index: 2, name: 'eth0', kind: 'ether', operationalState: 'routable', addresses: ['192.168.1.24/24'] }] },
  },
  '/api/v1/network/status': {
    interfaces: { available: true, count: 0, entries: [] },
    defaultRoutes: { available: true, count: 0, entries: [] },
    dns: { available: true, linkServers: [], resolverServers: [] },
    wifi: { available: true, associations: [] },
    capabilities: { wifi: { supported: false, interfaces: [] }, bluetooth: { supported: false, adapters: [] }, cellular: { supported: false, interfaces: [] } },
  },
  '/api/v1/wifi/client': { enabled: true, interface: 'wlan0' },
  '/api/v1/wifi/ap': {
    mode: 'provisioning',
    interface: 'wlan0',
    ssid: 'mica-lab',
    psk: '<redacted>',
    channel: 11,
    countryCode: 'DE',
    address: '192.168.4.1/24',
    holdDownSeconds: 120,
    graceSeconds: 60,
  },
  '/api/v1/state/network': {},
  '/api/v1/wifi/client/networks': [{ ssid: 'workshop', psk: 'x', hidden: false, priority: 10 }],
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the network page', () => {
  it('gives each interface one link and no second navigation behind it', async () => {
    // The row used to carry an onClick to the same route as the link it
    // contained, so a click on the name navigated twice and the keyboard
    // reached neither.
    stubFetch(routes)
    renderRoute(<NetworkPage />)

    const link = await screen.findByRole('link', { name: /eth0/ })
    expect(link.getAttribute('href')).toContain('/network/eth0')
    const row = link.closest('tr')!
    expect(within(row).getAllByRole('link')).toHaveLength(1)
  })

  it('closes the Wi-Fi removal confirmation and names the network', async () => {
    stubFetch({ ...routes, 'DELETE /api/v1/wifi/client/networks/workshop': () => new Response(null, { status: 204 }) })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('tab', { name: 'Known Wi-Fi' }))
    await userEvent.click(await screen.findByRole('button', { name: 'Remove workshop' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Remove workshop' }))

    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect((await screen.findAllByText('Network workshop removed.')).length).toBeGreaterThan(0)
  })

  /// The operator was never shown the key, so an empty password box means
  /// "keep it" and the request must carry no `psk` at all.
  it('edits a stored network without sending a key it never showed', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/wifi/client/networks/workshop': () => jsonResponse({ ssid: 'workshop', psk: '<redacted>', hidden: true, priority: 3 }) })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('tab', { name: 'Known Wi-Fi' }))
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    const priority = within(dialog).getByLabelText('Priority')
    await userEvent.clear(priority)
    await userEvent.type(priority, '3')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ ssid: 'workshop', hidden: false, priority: 3 })
  })

  /// A scan is a POST: it sweeps every channel and briefly costs the station
  /// its link, so it happens when an operator asks for it.
  it('scans on request and offers to connect to what it found', async () => {
    const fetch = stubFetch({
      ...routes,
      'POST /api/v1/wifi/client/scan': () => jsonResponse({
        available: true,
        interface: 'wlan0',
        networks: [{ ssid: 'lab-5g', bssid: 'aa:bb:cc:dd:ee:01', signalDbm: -42, flags: '[WPA2-PSK-CCMP][ESS]' }],
      }),
    })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('tab', { name: 'Known Wi-Fi' }))
    expect(fetch.mock.calls.some(([path]) => String(path) === '/api/v1/wifi/client/scan')).toBe(false)

    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))

    expect(await screen.findByText('lab-5g')).toBeTruthy()
    expect(screen.getByText('-42 dBm')).toBeTruthy()
    // Connecting is adding the network: the device joins what it is told to
    // join, and the dialog opens with the name already filled in.
    await userEvent.click(screen.getByRole('button', { name: 'Connect' }))
    // By name: a toast is a dialog too, and the success toast from the scan is
    // still on screen.
    const dialog = await screen.findByRole('dialog', { name: 'Add Wi-Fi network' })
    expect((within(dialog).getByLabelText('SSID') as HTMLInputElement).value).toBe('lab-5g')
  })

  /// The access point is configured from the same tab, and its key is never
  /// shown: the field is empty and an empty field keeps the stored one.
  it('configures the access point without ever showing its key', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/wifi/ap': () => jsonResponse({ taskId: 'task-2' }, 202) })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('tab', { name: 'Known Wi-Fi' }))
    const key = await screen.findByLabelText(/Pre-shared key/)
    expect((key as HTMLInputElement).value).toBe('')
    await userEvent.click(screen.getByRole('button', { name: 'Save access point' }))

    const call = await waitFor(() => {
      // The PUT, not the GET the panel read its values with.
      const found = fetch.mock.calls.find(([path, init]) =>
        String(path) === '/api/v1/wifi/ap' && (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    const body = JSON.parse(String((call?.[1] as RequestInit).body))
    expect(body.psk).toBeUndefined()
    expect(body.mode).toBe('provisioning')
  })

  it('reports the Wi-Fi client switch, which used to change nothing visible', async () => {
    stubFetch({ ...routes, 'PUT /api/v1/wifi/client': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('tab', { name: 'Known Wi-Fi' }))
    await userEvent.click(await screen.findByRole('switch', { name: 'Wi-Fi client' }))

    expect((await screen.findAllByText('Wi-Fi client disabled.')).length).toBeGreaterThan(0)
  })

  it('keeps the add-interface dialog reachable when static addressing expands it', async () => {
    stubFetch(routes)
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    await userEvent.click(await screen.findByRole('switch', { name: 'Use DHCP' }))

    const dialog = await screen.findByRole('dialog')
    // Header and footer stay outside the scroll region, so both are present
    // however tall the body grows.
    expect(within(dialog).getByText('Add network interface')).toBeTruthy()
    expect(within(dialog).getByRole('button', { name: 'Add' })).toBeTruthy()
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeTruthy()
    expect(within(dialog).getByLabelText('Address')).toBeTruthy()
  })
})

describe('the add-interface dialog', () => {
  it.each([
    ['VLAN', ['Parent interface', 'VLAN ID']],
    ['Bridge', ['Bridge ports']],
    ['WireGuard', ['Listen port']],
  ])('asks only for what a %s interface needs', async (kind, fields) => {
    stubFetch(routes)
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.click(within(dialog).getByRole('combobox', { name: 'Interface kind' }))
    await userEvent.click(await screen.findByRole('option', { name: kind }))

    for (const field of fields) {
      expect(within(await screen.findByRole('dialog')).getByLabelText(field)).toBeTruthy()
    }
  })

  it('sends the typed interface and reports it', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/network/eth1': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderRoute(<NetworkPage />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Interface name'), 'eth1')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Add' }))

    expect((await screen.findAllByText('Interface eth1 saved.')).length).toBeGreaterThan(0)
    const put = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')!
    expect(JSON.parse(String((put[1] as RequestInit).body))).toEqual({ dhcp: true })
  })
})
