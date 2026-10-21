import { act, cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { accessPoint, configured, networkRoutes, observed } from '@/shared/testing/network'
import { resetChangeSet, updateChangeSet } from './change-set'
import { stageSet } from './model'
import { NetworkPage } from './network-page'

afterEach(() => {
  cleanup()
  resetChangeSet()
  sessionStorage.clear()
  vi.unstubAllGlobals()
})

const open = (tab?: string) => renderParamRoute(<NetworkPage />, '/network', tab ? `/network?tab=${tab}` : '/network')

const bodyOf = (fetch: ReturnType<typeof stubFetch>, method: string, path: string) => {
  const call = fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
  return call ? JSON.parse(String((call[1] as RequestInit).body)) as Record<string, unknown> : undefined
}

describe('the network page', () => {
  it('shows only the tabs the device serves', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/meta': { api: 'v1', features: [], init: 'systemd' } })
    open()
    await screen.findByRole('tab', { name: 'Interfaces' })
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'Wi-Fi' })).toBeNull())
    expect(screen.queryByRole('tab', { name: 'Bluetooth' })).toBeNull()
    expect(screen.getByRole('tab', { name: 'WireGuard' })).toBeTruthy()
    expect(screen.getByRole('tab', { name: 'Overview' })).toBeTruthy()
    expect(screen.getByRole('tab', { name: 'DNS' })).toBeTruthy()
  })

  it('offers no tunnels and no new interfaces on OpenRC', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/meta': { api: 'v1', features: ['wifi'], init: 'openrc' } })
    open()
    await screen.findByRole('link', { name: 'eth0' })
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'WireGuard' })).toBeNull())
    expect(screen.queryByRole('button', { name: 'Add' })).toBeNull()
  })

  it('groups the interfaces by role and leaves the loopback out', async () => {
    stubFetch(networkRoutes())
    open()
    const ports = await screen.findByRole('region', { name: 'Ethernet ports' })
    expect(within(ports).getAllByRole('link').map((card) => card.getAttribute('aria-label'))).toEqual(['eth0', 'eth1', 'eth2'])
    expect(within(within(ports).getByRole('link', { name: 'eth0' })).getByText('Exit')).toBeTruthy()
    expect(within(within(ports).getByRole('link', { name: 'eth2' })).getAllByText('No cable').length).toBeGreaterThan(0)
    expect(screen.getByRole('region', { name: 'Wireless' })).toBeTruthy()
    expect(screen.getByRole('region', { name: 'Tunnels' })).toBeTruthy()
    expect(screen.getByRole('region', { name: 'Field buses' })).toBeTruthy()
    expect(screen.queryByRole('link', { name: 'lo' })).toBeNull()
  })

  it('stages a tunnel, reviews it, and writes the whole map in one request', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/network': () => jsonResponse({ taskId: 't1' }),
      '/api/v1/tasks/t1': { id: 't1', operation: 'network', dotPath: 'network', source: 'api', status: 'finished', outcome: 'succeeded', enqueuedAt: '', foldedCount: 0 },
      '/api/v1/health': { apid: 'ok', micad: 'ok' },
    })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add' }))
    const wizard = await screen.findByRole('dialog')
    await userEvent.click(within(wizard).getByRole('button', { name: 'Stage' }))

    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    expect(within(bar).getByText('1 pending change')).toBeTruthy()
    await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))

    const review = await screen.findByRole('dialog')
    expect(within(review).getByText('wg1')).toBeTruthy()
    // Where the browser came from is not known here, so the write asks first.
    const apply = within(review).getByRole('button', { name: 'Apply 1 change' })
    expect(apply.hasAttribute('disabled')).toBe(true)
    await userEvent.click(within(review).getByRole('switch'))
    await userEvent.click(apply)

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/network')).toBeTruthy())
    expect(bodyOf(fetch, 'PUT', '/api/v1/network')).toEqual({ ...configured(), wg1: { kind: 'wireguard', static: { address: '10.2.0.1/24' }, wireguard: { listenPort: 51821, peers: [] } } })
    expect(await screen.findByText('The device reports the interfaces reconciled.')).toBeTruthy()
  })

  it('reports a change the device made underneath the staged edits instead of overwriting it', async () => {
    let reads = 0
    const routes = networkRoutes()
    const fetch = stubFetch({
      ...routes,
      '/api/v1/network': () => {
        reads += 1
        const map = configured()
        if (reads > 1) map.eth1 = { dhcp: true }
        return jsonResponse({ ...(routes['/api/v1/network'] as object), configured: map })
      },
    })
    open()
    await screen.findByRole('link', { name: 'eth1' })
    act(() => updateChangeSet((set) => stageSet(set, 'eth1', 'physical', (entry) => ({ ...entry, static: { address: '192.168.20.1/24' } }))))
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))
    const review = await screen.findByRole('dialog')
    await userEvent.click(within(review).getByRole('switch'))
    await userEvent.click(within(review).getByRole('button', { name: 'Apply 1 change' }))

    expect(await screen.findByText(/The device configuration changed underneath these edits: eth1/)).toBeTruthy()
    expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(false)
  })

  it('summarises the device on the overview tab', async () => {
    stubFetch(networkRoutes())
    open('overview')
    expect(await screen.findByText('Online')).toBeTruthy()
    expect(screen.getByText('via 192.168.1.1')).toBeTruthy()
    expect(screen.getByText('Uplink eth0')).toBeTruthy()
  })

  it('shows what the device resolves with on the DNS tab and checks again on request', async () => {
    const fetch = stubFetch({ ...networkRoutes(), 'POST /api/v1/dns/probe': () => jsonResponse({ ok: false, name: 'example.org', latencyMs: null, checkedAt: '2026-10-21T08:01:00Z' }) })
    open('dns')
    expect(await screen.findByText('Resolves · 18 ms')).toBeTruthy()
    expect(screen.getByText('example.org')).toBeTruthy()
    expect(screen.getByText('(static)')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Check again' }))
    await waitFor(() => expect(fetch.mock.calls.some(([input, init]) => String(input) === '/api/v1/dns/probe' && init?.method === 'POST')).toBe(true))
  })

  it('tests the global servers and turns them on after a confirmation', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'POST /api/v1/dns/test': () => jsonResponse([{ server: '9.9.9.9', reachable: true, latencyMs: 14 }, { server: '1.1.1.1', reachable: false, latencyMs: null }]),
      'PUT /api/v1/dns/global': () => jsonResponse({ taskId: 't1' }, 202),
    })
    open('dns')
    await screen.findByText('nas.lan')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])
    await userEvent.click(screen.getByRole('switch', { name: 'Global DNS' }))
    expect(screen.getByLabelText('Preferred DNS')).toHaveProperty('value', '9.9.9.9')
    await userEvent.type(screen.getByLabelText('Alternate DNS 1'), '1.1.1.1')
    await userEvent.click(screen.getByRole('button', { name: 'Test servers' }))
    expect(await screen.findByText('Reachable · 14 ms')).toBeTruthy()
    expect(screen.getByText('Unreachable (3 s timeout)')).toBeTruthy()
    expect(bodyOf(fetch, 'POST', '/api/v1/dns/test')).toEqual({ servers: ['9.9.9.9', '1.1.1.1'] })
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Unreachable in the test: 1.1.1.1. You can still save.')).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/dns/global')).toEqual({ enabled: true, servers: ['9.9.9.9', '1.1.1.1'] }))
  })

  it('refuses a server the device cannot ask', async () => {
    stubFetch(networkRoutes())
    open('dns')
    await screen.findByText('nas.lan')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])
    await userEvent.click(screen.getByRole('switch', { name: 'Global DNS' }))
    await userEvent.type(screen.getByLabelText('Alternate DNS 1'), '127.0.0.53')
    expect(screen.getByText('Not a loopback address')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', true)
  })

  it('adds, edits and removes a static record by its id', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'POST /api/v1/dns/static-records': () => jsonResponse({ id: 'r2', hostname: 'printer.lan', address: '192.168.1.30' }, 201),
      'PUT /api/v1/dns/static-records/r1': () => jsonResponse({ id: 'r1', hostname: 'nas.lan', address: '192.168.1.21' }),
      'DELETE /api/v1/dns/static-records/r1': () => new Response(null, { status: 204 }),
    })
    open('dns')
    await screen.findByText('nas.lan')
    await userEvent.click(screen.getByRole('button', { name: 'Add record' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Name'), 'mica-bench')
    expect(within(dialog).getByText('Neither localhost nor this device\'s hostname')).toBeTruthy()
    await userEvent.clear(within(dialog).getByLabelText('Name'))
    await userEvent.type(within(dialog).getByLabelText('Name'), 'Printer.lan')
    await userEvent.type(within(dialog).getByLabelText('IPv4 address'), '192.168.1.30')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'POST', '/api/v1/dns/static-records')).toEqual({ hostname: 'printer.lan', address: '192.168.1.30' }))

    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Add record' })).toBeNull())
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' }).at(-1)!)
    const edit = await screen.findByRole('dialog', { name: 'Edit record' })
    await userEvent.clear(within(edit).getByLabelText('IPv4 address'))
    await userEvent.type(within(edit).getByLabelText('IPv4 address'), '192.168.1.21')
    await userEvent.click(within(edit).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/dns/static-records/r1')).toEqual({ hostname: 'nas.lan', address: '192.168.1.21', comment: 'Storage' }))

    await userEvent.click(screen.getByRole('button', { name: 'Delete nas.lan?' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Delete' }))
    await waitFor(() => expect(fetch.mock.calls.some(([input, init]) => String(input) === '/api/v1/dns/static-records/r1' && init?.method === 'DELETE')).toBe(true))
  })
})

describe('the Wi-Fi tab', () => {
  it('edits a saved network without sending a key it never showed', async () => {
    const fetch = stubFetch({ ...networkRoutes(), 'PUT /api/v1/wifi/client/networks/marina': () => jsonResponse({ ssid: 'marina', psk: '<redacted>', hidden: false, priority: 3 }) })
    open('wifi')
    await screen.findByText('marina')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' })[0])
    const dialog = await screen.findByRole('dialog')
    const priority = within(dialog).getByLabelText('Priority')
    await userEvent.clear(priority)
    await userEvent.type(priority, '3')
    await userEvent.click(within(dialog).getByRole('switch', { name: 'Auto-connect' }))
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi/client/networks/marina')).toEqual({ ssid: 'marina', hidden: false, priority: 3, autoConnect: false }))
  })

  it('removes a saved network behind a confirmation', async () => {
    stubFetch({ ...networkRoutes(), 'DELETE /api/v1/wifi/client/networks/marina': () => new Response(null, { status: 204 }) })
    open('wifi')
    await userEvent.click(await screen.findByRole('button', { name: 'Remove marina' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Remove' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect((await screen.findAllByText('Network marina removed.')).length).toBeGreaterThan(0)
  })

  it('does not scan while the client is off', async () => {
    stubFetch(networkRoutes())
    open('wifi')
    expect(await screen.findByText('The Wi-Fi client is off. Turn it on to scan.')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Scan' }).hasAttribute('disabled')).toBe(true)
  })

  it('starts the access point with one write of both roles', async () => {
    const fetch = stubFetch({ ...networkRoutes(), 'PATCH /api/v1/settings': () => jsonResponse({ taskId: 'roles' }, 202) })
    open('wifi')
    await screen.findByText('marina')
    // The saved network's row comes first; the radio's own card is last.
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' }).at(-1)!)
    const config = screen.getByRole('switch', { name: 'Wi-Fi' })
    await userEvent.click(config)
    await userEvent.click(screen.getByRole('button', { name: 'Access point' }))
    await userEvent.click(screen.getByRole('checkbox', { name: 'The device operates in this country' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PATCH', '/api/v1/settings')).toEqual({ 'wifi.client.enabled': false, 'wifi.ap.mode': 'always' }))
    expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(false)
  })

  it('writes the access point\'s security, pool and lease without sending its stored key', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/wifi/ap': () => jsonResponse({ taskId: 'ap' }),
      'PATCH /api/v1/settings': () => jsonResponse({ taskId: 'roles' }, 202),
    })
    open('wifi')
    await screen.findByText('marina')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' }).at(-1)!)
    await userEvent.click(screen.getByRole('switch', { name: 'Wi-Fi' }))
    await userEvent.click(screen.getByRole('button', { name: 'Access point' }))
    await userEvent.click(screen.getByRole('checkbox', { name: 'The device operates in this country' }))
    expect(screen.getByLabelText('Pool start')).toHaveProperty('value', '192.168.4.2')
    expect(screen.getByLabelText('Pool end')).toHaveProperty('value', '192.168.4.254')
    await userEvent.clear(screen.getByLabelText('Pool start'))
    await userEvent.type(screen.getByLabelText('Pool start'), '192.168.4.1')
    expect(screen.getByText('The pool cannot include the access point\'s address 192.168.4.1')).toBeTruthy()
    await userEvent.clear(screen.getByLabelText('Pool start'))
    await userEvent.type(screen.getByLabelText('Pool start'), '192.168.4.100')
    await userEvent.clear(screen.getByLabelText('Pool end'))
    await userEvent.type(screen.getByLabelText('Pool end'), '192.168.4.149')
    await userEvent.type(screen.getByLabelText('Lease (seconds)'), '3600')
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PATCH', '/api/v1/settings')).toEqual({ 'wifi.client.enabled': false, 'wifi.ap.mode': 'always' }))
    const body = bodyOf(fetch, 'PUT', '/api/v1/wifi/ap')!
    expect(body.psk).toBeUndefined()
    expect(body).toMatchObject({ mode: 'off', interface: 'wlan0', ssid: 'mica-lab', countryCode: 'DE', channel: 6, address: '192.168.4.1/24', security: 'wpa2-psk', pool: { offset: 100, size: 50 }, leaseSeconds: 3600 })
    const order = fetch.mock.calls.filter(([, init]) => ['PUT', 'PATCH'].includes((init as RequestInit | undefined)?.method ?? '')).map(([input]) => String(input))
    expect(order).toEqual(['/api/v1/wifi/ap', '/api/v1/settings'])
  })

  it('asks before switching a running access point to the client, and lets go of the radio first', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      '/api/v1/wifi/ap': accessPoint('always'),
      'PATCH /api/v1/settings': () => jsonResponse({ taskId: 'roles' }, 202),
    })
    open('wifi')
    expect(await screen.findByText('No clients are connected.')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Client' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))

    const confirm = await screen.findByRole('alertdialog')
    expect(within(confirm).getByText('Switch to client?')).toBeTruthy()
    await userEvent.click(within(confirm).getByRole('button', { name: 'Switch and save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PATCH', '/api/v1/settings')).toEqual({ 'wifi.client.enabled': true, 'wifi.ap.mode': 'off' }))
    expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(false)
  })
})

describe('the Wi-Fi tab as a client', () => {
  const joined = () => {
    const status = observed({ wifi: { available: true, associations: [{ interface: 'wlan0', associated: true, ssid: 'marina', rssiDbm: -52 }] } })
    return { ...networkRoutes(configured(), status), '/api/v1/wifi/client': { enabled: true, interface: 'wlan0' } }
  }

  it('shows the network it joined and connects to one it found', async () => {
    const fetch = stubFetch({
      ...joined(),
      'POST /api/v1/wifi/client/scan': () => jsonResponse({ available: true, interface: 'wlan0', networks: [
        { ssid: 'harbour', bssid: '02:00:00:00:01:01', signalDbm: -60, frequencyMhz: 2437, flags: '[WPA2-PSK-CCMP][ESS]' },
        { ssid: 'harbour', bssid: '02:00:00:00:01:02', signalDbm: -70, frequencyMhz: 5180, flags: '[WPA2-PSK-CCMP][ESS]' },
        { ssid: 'office', bssid: '02:00:00:00:01:03', signalDbm: -80, frequencyMhz: 2412, flags: '[WPA2-EAP-CCMP][ESS]' },
      ] }),
      'POST /api/v1/wifi/client/networks': () => jsonResponse({ ssid: 'harbour', psk: '<redacted>', hidden: false, priority: 0 }),
    })
    open('wifi')
    expect(await screen.findByText('-52 dBm')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))
    expect(await screen.findByText('harbour')).toBeTruthy()
    expect(screen.getByText('2.4 GHz / 5 GHz')).toBeTruthy()
    expect(screen.getAllByRole('button', { name: 'Connect' })).toHaveLength(1)
    await userEvent.click(screen.getByRole('button', { name: 'Connect' }))
    const dialog = await screen.findByRole('dialog', { name: 'Add network' })
    await userEvent.type(within(dialog).getByLabelText('Password'), 'short')
    expect(within(dialog).getAllByText('8 to 63 characters.').length).toBeGreaterThan(0)
    await userEvent.type(within(dialog).getByLabelText('Password'), '-enough')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'POST', '/api/v1/wifi/client/networks')).toEqual({ ssid: 'harbour', psk: 'short-enough', hidden: false, priority: 0 }))
  })

  it('stages nothing and asks before turning the radio off', async () => {
    const fetch = stubFetch({ ...joined(), 'PATCH /api/v1/settings': () => jsonResponse({ taskId: 'off' }, 202) })
    open('wifi')
    await screen.findByText('-52 dBm')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' }).at(-1)!)
    await userEvent.click(screen.getByRole('switch', { name: 'Wi-Fi' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))
    const confirm = await screen.findByRole('alertdialog')
    await userEvent.click(within(confirm).getByRole('button', { name: 'Turn off' }))
    await waitFor(() => expect(bodyOf(fetch, 'PATCH', '/api/v1/settings')).toEqual({ 'wifi.client.enabled': false, 'wifi.ap.mode': 'off' }))
  })

  it('writes the radio\'s own addressing as an interface', async () => {
    const fetch = stubFetch({ ...joined(), 'PUT /api/v1/network/wlan0': () => jsonResponse({ taskId: 'w' }) })
    open('wifi')
    await screen.findByText('-52 dBm')
    await userEvent.click(screen.getAllByRole('button', { name: 'Edit' }).at(-1)!)
    await userEvent.click(screen.getByRole('button', { name: 'Static' }))
    await userEvent.type(screen.getByLabelText('Address'), '192.168.30.5/24')
    await userEvent.type(screen.getByLabelText('Gateway'), '10.0.0.1')
    const bar = screen.getByRole('region', { name: /Editing/ })
    expect(await screen.findByText('The gateway is outside this subnet.')).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true)
    await userEvent.clear(screen.getByLabelText('Gateway'))
    await userEvent.type(screen.getByLabelText('Gateway'), '192.168.30.1')
    await userEvent.click(within(bar).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/network/wlan0')).toEqual({ dhcp: false, static: { address: '192.168.30.5/24', gateway: '192.168.30.1', dns: [] } }))
  })
})

describe('the Wi-Fi tab as an access point', () => {
  it('lists the connected stations and refuses an unconfirmed country or a long SSID', async () => {
    const status = observed({ accessPoint: { available: true, entries: [{ interface: 'wlan0', stationCount: 1, stations: [{ mac: '02:00:00:00:02:01', signalDbm: -48, connectedSeconds: 600, rxBytes: 2_500_000, txBytes: 1_200 }] }] } })
    stubFetch({ ...networkRoutes(configured(), status), '/api/v1/wifi/ap': accessPoint('always') })
    open('wifi')
    expect(await screen.findByText('02:00:00:00:02:01')).toBeTruthy()
    expect(screen.getByText('10 min')).toBeTruthy()
    expect(screen.getByText('2.5 MB / 1 KB')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit' }))
    const ssid = screen.getByLabelText('SSID')
    await userEvent.clear(ssid)
    await userEvent.type(ssid, 'x'.repeat(33))
    expect(await screen.findByText('An SSID is at most 32 bytes.')).toBeTruthy()
    expect(screen.getByText('Confirm the country the device operates in.')).toBeTruthy()
    const bar = screen.getByRole('region', { name: /Editing/ })
    expect(within(bar).getByText('2 problems to fix')).toBeTruthy()
  })
})

describe('the add-interface wizard', () => {
  it('names a VLAN after its parent and refuses an ID out of range', async () => {
    stubFetch(networkRoutes())
    open()
    await userEvent.click(await screen.findByRole('button', { name: 'Add' }))
    const wizard = await screen.findByRole('dialog')
    await userEvent.click(within(wizard).getByRole('button', { name: 'VLAN' }))
    const id = within(wizard).getByLabelText('VLAN ID')
    await userEvent.type(id, '5000')
    expect(within(wizard).getByText('A VLAN ID is 1 to 4094.')).toBeTruthy()
    expect(within(wizard).getByRole('button', { name: 'Stage' }).hasAttribute('disabled')).toBe(true)
    await userEvent.clear(id)
    await userEvent.type(id, '20')
    expect((within(wizard).getByLabelText('Name') as HTMLInputElement).value).toBe('eth0-20')
  })
})
