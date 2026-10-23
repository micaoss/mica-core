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
    expect(screen.getByRole('tab', { name: 'Global DNS' })).toBeTruthy()
  })

  it('offers no tunnels and no new interfaces on OpenRC', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/meta': { api: 'v1', features: ['wifi'], init: 'openrc' } })
    open()
    await screen.findByRole('link', { name: 'eth0' })
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'WireGuard' })).toBeNull())
    expect(screen.queryByRole('button', { name: 'Add interface' })).toBeNull()
  })

  it('groups the interfaces by role and leaves the loopback out', async () => {
    stubFetch(networkRoutes())
    open()
    const ports = await screen.findByRole('region', { name: 'Ethernet ports' })
    expect(within(ports).getAllByRole('link').map((card) => card.getAttribute('aria-label'))).toEqual(['eth0', 'eth1', 'eth2'])
    expect(within(within(ports).getByRole('link', { name: 'eth0' })).getByText('Exit')).toBeTruthy()
    expect(within(within(ports).getByRole('link', { name: 'eth2' })).getAllByText('No cable').length).toBeGreaterThan(0)
    expect(within(within(ports).getByRole('link', { name: 'eth2' })).queryByText('Waiting for DHCP')).toBeNull()
    expect(screen.getByRole('region', { name: 'Wireless' })).toBeTruthy()
    expect(screen.getByRole('region', { name: 'Tunnels' })).toBeTruthy()
    expect(screen.getByRole('region', { name: 'Field buses' })).toBeTruthy()
    expect(screen.queryByRole('link', { name: 'lo' })).toBeNull()
  })

  it('shows a CAN bus by its state, bit rate and error counters', async () => {
    stubFetch(networkRoutes())
    open()
    const card = within(await screen.findByRole('region', { name: 'Field buses' })).getByRole('link', { name: 'can0' })
    expect(within(card).getByText('250 kbit/s')).toBeTruthy()
    expect(within(card).getByText('TX 3 / RX 0')).toBeTruthy()
  })

  it('stages a tunnel, reviews it, and writes the whole map in one request', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/network': () => jsonResponse({ taskId: 't1' }),
      '/api/v1/tasks/t1': { id: 't1', operation: 'network', dotPath: 'network', source: 'api', status: 'finished', outcome: 'succeeded', enqueuedAt: '', foldedCount: 0 },
      '/api/v1/health': { apid: 'ok', micad: 'ok' },
    })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
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
    expect(bodyOf(fetch, 'PUT', '/api/v1/network')).toEqual({ baseVersion: 'v1', configured: { ...configured(), wg1: { kind: 'wireguard', static: { address: '10.2.0.1/24' }, wireguard: { listenPort: 51821, peers: [] } } } })
    expect(await screen.findByText('The device reports the interfaces reconciled.')).toBeTruthy()
  })

  it('offers a new tunnel a port no service on the device holds', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/network/wireguard/defaults': { name: 'wg1', address: '10.2.0.1/24', listenPort: 51822, udpPortsInUse: [51821] } })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    const wizard = await screen.findByRole('dialog')
    const port = within(wizard).getByLabelText('Listen port')
    await waitFor(() => expect((port as HTMLInputElement).value).toBe('51822'))
    expect((within(wizard).getByLabelText('Tunnel address') as HTMLInputElement).value).toBe('10.2.0.1/24')
    await userEvent.clear(port)
    await userEvent.type(port, '51821')
    expect(within(wizard).getByText('A service on the device listens on that port.')).toBeTruthy()
    expect(within(wizard).getByRole('button', { name: 'Stage' }).hasAttribute('disabled')).toBe(true)
  })

  it('restores the previous map at the version it reads first when the apply fails', async () => {
    let puts = 0
    const fetch = stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/network': () => { puts += 1; return jsonResponse({ taskId: `t${puts}` }) },
      '/api/v1/tasks/t1': { id: 't1', operation: 'network', dotPath: 'network', source: 'api', status: 'finished', outcome: 'failed', message: 'wg1 did not come up', enqueuedAt: '', foldedCount: 0 },
      '/api/v1/tasks/t2': { id: 't2', operation: 'network', dotPath: 'network', source: 'api', status: 'running', enqueuedAt: '', foldedCount: 0 },
      '/api/v1/health': { apid: 'ok', micad: 'ok' },
    })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Stage' }))
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))
    const review = await screen.findByRole('dialog')
    await userEvent.click(within(review).getByRole('switch'))
    await userEvent.click(within(review).getByRole('button', { name: 'Apply 1 change' }))

    await userEvent.click(await screen.findByRole('button', { name: 'Restore previous configuration' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Restore previous configuration' }))
    await waitFor(() => expect(puts).toBe(2))
    const writes = fetch.mock.calls.filter(([input, init]) => String(input) === '/api/v1/network' && (init as RequestInit | undefined)?.method === 'PUT')
    expect(JSON.parse(String((writes[1][1] as RequestInit).body))).toEqual({ baseVersion: 'v1', configured: configured() })
  })

  it('keeps the changes and says so when the map moved between the read and the write', async () => {
    stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/network': () => jsonResponse({ error: { code: 'version_conflict', message: 'the network map changed since it was read', path: 'network' } }, 409),
    })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Stage' }))
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))
    const review = await screen.findByRole('dialog')
    await userEvent.click(within(review).getByRole('switch'))
    await userEvent.click(within(review).getByRole('button', { name: 'Apply 1 change' }))
    expect(await within(review).findByText(/changed while this was being applied/)).toBeTruthy()
    expect(within(review).getByText('wg1')).toBeTruthy()
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
    // The review's dry run writes nothing; the write itself never went out.
    expect(fetch.mock.calls.some(([input, init]) => String(input) === '/api/v1/network' && (init as RequestInit | undefined)?.method === 'PUT')).toBe(false)
  })

  it('shows the parent a write takes over and marks each interface a refusal names', async () => {
    stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/network?dryRun=true': { adopted: ['eth2'], warnings: [], diff: [] },
      'PUT /api/v1/network': () => jsonResponse({ error: {
        code: 'vlan_parent_unsupported', message: 'the default configuration of eth2 cannot be taken over', path: 'network',
        params: { parent: 'eth2', reason: '[Network] Bond=' },
        details: [{ interface: 'wg1', field: 'vlan.parent', code: 'vlan_parent_unsupported', params: { parent: 'eth2', reason: '[Network] Bond=' } }],
      } }, 422),
    })
    open('wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Stage' }))
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))
    const review = await screen.findByRole('dialog')
    expect(await within(review).findByText('eth2 is declared with the configuration it runs now, so it can carry the VLAN.')).toBeTruthy()
    await userEvent.click(within(review).getByRole('switch'))
    await userEvent.click(within(review).getByRole('button', { name: 'Apply 1 change' }))
    expect(await within(review).findByText('The default configuration of eth2 cannot be taken over by mica ([Network] Bond=); configure eth2 first.')).toBeTruthy()
    expect(within(review).getByText('Refused')).toBeTruthy()
  })

  it('summarises the device on the overview tab', async () => {
    stubFetch(networkRoutes())
    open('overview')
    expect(await screen.findByText('Online')).toBeTruthy()
    expect(screen.getByText('via 192.168.1.1')).toBeTruthy()
    expect(screen.getByText('Uplink eth0')).toBeTruthy()
  })

  it('lists every IPv4 route of the main table with its source on the overview', async () => {
    stubFetch(networkRoutes())
    open('overview')
    expect(await screen.findByText('Routes · 6 · default via eth0')).toBeTruthy()
    const rows = within(screen.getByRole('table')).getAllByRole('row').slice(1)
    expect(rows.map((row) => within(row).getAllByRole('cell').map((cell) => cell.textContent))).toEqual([
      ['default', '192.168.1.1', 'eth0', '1024', 'DHCP'],
      ['192.168.1.1/32', 'on-link', 'eth0', '1024', 'DHCP'],
      ['192.168.1.0/24', 'on-link', 'eth0', '1024', 'On link'],
      ['192.168.10.0/24', 'on-link', 'eth1', '0', 'On link'],
      ['10.20.0.0/16', '192.168.10.2', 'eth1', '0', 'Static'],
      ['10.8.0.0/24', 'on-link', 'wg0', '0', 'WireGuard'],
    ])
    expect(screen.queryByText('fe80::/64')).toBeNull()
  })

  it('shows the first 20 routes and the rest on request', async () => {
    const status = observed()
    const eth1 = status.interfaces.entries!.find((entry) => entry.name === 'eth1')!
    for (let at = 0; at < 20; at++) eth1.routes!.push({ dest: `10.${100 + at}.0.0/16`, via: '192.168.10.2', source: 'static', protocol: 'static', table: 'main', family: 'ipv4' })
    stubFetch(networkRoutes(configured(), status))
    open('overview')
    expect(await screen.findByText('Routes · 26 · default via eth0')).toBeTruthy()
    expect(within(screen.getByRole('table')).getAllByRole('row')).toHaveLength(21)
    await userEvent.click(screen.getByRole('button', { name: 'Show all 26 routes' }))
    expect(within(screen.getByRole('table')).getAllByRole('row')).toHaveLength(27)
  })

  it('shows each default route as an uplink, the one in use first, and a joined Wi-Fi client among them', async () => {
    const status = observed()
    const wlan = status.interfaces.entries!.find((entry) => entry.name === 'wlan0')!
    wlan.link = { carrier: true, operationalState: 'routable' }
    wlan.routes = [{ dest: '0.0.0.0/0', via: '192.168.1.1', metric: 600, source: 'DHCPv4', protocol: 'dhcp', table: 'main', family: 'ipv4' }]
    status.wifi.associations = [{ interface: 'wlan0', associated: true, ssid: 'marina' }]
    status.defaultRoutes.entries = [
      { family: 'ipv4', gateway: '192.168.1.1', interface: 'wlan0', metric: 600, effective: false },
      { family: 'ipv4', gateway: '192.168.1.1', interface: 'eth0', metric: 1024, effective: true },
    ]
    stubFetch({ ...networkRoutes(configured(), status), '/api/v1/wifi/client': { enabled: true, interface: 'wlan0' }, '/api/v1/wifi/ap': accessPoint('off') })
    open('overview')
    expect(await screen.findByText('Uplink eth0')).toBeTruthy()
    expect(screen.getByText('Standby wlan0')).toBeTruthy()
    expect(screen.queryByText('Joined marina')).toBeNull()
    const defaults = within(screen.getByRole('table')).getAllByRole('row').slice(1, 3)
    expect(defaults.map((row) => [within(row).getAllByRole('cell')[2].textContent, within(row).getByText(/In use|Standby/).textContent])).toEqual([['eth0', 'In use'], ['wlan0', 'Standby']])
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
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
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
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
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

  it('locks the Wi-Fi configuration while a Bluetooth provisioning session holds the radio', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/bluetooth/provisioning': { state: 'connected', trigger: 'boot', expiresAt: null, autoWhenOffline: true, verifierReady: true, lockedUntil: null, wifiLocked: true, history: [] } })
    open('wifi')
    expect(await screen.findByText('Bluetooth provisioning in progress; the Wi-Fi configuration is locked.')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Remove marina' }).hasAttribute('disabled')).toBe(true)
    expect(screen.getAllByRole('button', { name: /^Edit/ }).every((button) => button.hasAttribute('disabled'))).toBe(true)
  })

  it('does not scan while the client is off', async () => {
    stubFetch(networkRoutes())
    open('wifi')
    expect(await screen.findByText('The Wi-Fi client is off. Turn it on to scan.')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Scan' }).hasAttribute('disabled')).toBe(true)
  })

  it('asks for an access-point password when the device has none', async () => {
    const fetch = stubFetch({ ...networkRoutes(), '/api/v1/wifi/ap': { ...accessPoint(), psk: undefined }, 'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'roles' }, 202) })
    open('wifi')
    await screen.findByText('marina')
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('switch', { name: 'Wi-Fi' }))
    await userEvent.click(screen.getByRole('button', { name: 'Access point' }))
    await userEvent.click(screen.getByRole('checkbox', { name: 'The device operates in this country' }))
    expect(await screen.findByText('Required: this device has no access-point password yet.')).toBeTruthy()
    const editing = within(screen.getByRole('region', { name: /Editing/ }))
    expect(editing.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true)

    await userEvent.type(screen.getByLabelText('Password'), 'harbour-ap-key')
    await userEvent.click(editing.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect((bodyOf(fetch, 'PUT', '/api/v1/wifi') as { ap: { psk?: string } }).ap.psk).toBe('harbour-ap-key'))
  })

  it('starts the access point with one write of both roles', async () => {
    const fetch = stubFetch({ ...networkRoutes(), 'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'roles' }, 202) })
    open('wifi')
    await screen.findByText('marina')
    // The saved network's row comes first; the radio's own card is last.
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    const config = screen.getByRole('switch', { name: 'Wi-Fi' })
    await userEvent.click(config)
    await userEvent.click(screen.getByRole('button', { name: 'Access point' }))
    await userEvent.click(screen.getByRole('checkbox', { name: 'The device operates in this country' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi')).toEqual({ enabled: true, mode: 'ap', ap: { start: 'always', interface: 'wlan0', holdDownSeconds: 120, graceSeconds: 60, ssid: 'mica-lab', channel: 6, countryCode: 'DE', address: '192.168.4.1/24', security: 'wpa2-psk', dhcpServer: { poolStart: '192.168.4.2', poolEnd: '192.168.4.254' }, countryConfirmed: true } }))
    expect(fetch.mock.calls.filter(([, init]) => ['PUT', 'PATCH'].includes((init as RequestInit | undefined)?.method ?? ''))).toHaveLength(1)
  })

  it('writes the access point\'s security, pool and lease without sending its stored key', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'roles' }, 202),
    })
    open('wifi')
    await screen.findByText('marina')
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
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

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi')).toBeTruthy())
    const body = bodyOf(fetch, 'PUT', '/api/v1/wifi')!
    expect(body).toMatchObject({ enabled: true, mode: 'ap' })
    expect((body.ap as Record<string, unknown>).psk).toBeUndefined()
    expect(body.ap).toMatchObject({ start: 'always', interface: 'wlan0', ssid: 'mica-lab', countryCode: 'DE', countryConfirmed: true, channel: 6, address: '192.168.4.1/24', security: 'wpa2-psk', dhcpServer: { poolStart: '192.168.4.100', poolEnd: '192.168.4.149', leaseSeconds: 3600 } })
  })

  it('holds the channel to what the device says the country allows', async () => {
    stubFetch({ ...networkRoutes(), '/api/v1/wifi/channels?country=DE': { countryCode: 'DE', channels: [1, 2, 3, 4, 5], auto: true } })
    open('wifi')
    await screen.findByText('marina')
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('switch', { name: 'Wi-Fi' }))
    await userEvent.click(screen.getByRole('button', { name: 'Access point' }))
    expect(await screen.findByText('The chosen country does not allow this channel.')).toBeTruthy()
  })

  it('asks before switching a running access point to the client, and lets go of the radio first', async () => {
    const fetch = stubFetch({
      ...networkRoutes(),
      '/api/v1/wifi/ap': accessPoint('always'),
      'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'roles' }, 202),
    })
    open('wifi')
    expect(await screen.findByText('No clients are connected.')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Client' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))

    const confirm = await screen.findByRole('alertdialog')
    expect(within(confirm).getByText('Switch to client?')).toBeTruthy()
    await userEvent.click(within(confirm).getByRole('button', { name: 'Switch and save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi')).toEqual({ enabled: true, mode: 'client' }))
    expect(fetch.mock.calls.filter(([, init]) => ['PUT', 'PATCH'].includes((init as RequestInit | undefined)?.method ?? ''))).toHaveLength(1)
  })
})

describe('the Wi-Fi tab as a client', () => {
  const joined = () => {
    const status = observed({ wifi: { available: true, associations: [{ interface: 'wlan0', associated: true, ssid: 'marina', bssid: '02:00:00:00:03:01', frequencyMhz: 5180, keyManagement: 'SAE', rssiDbm: -52, linkSpeedMbps: 72 }] } })
    const wlan = status.interfaces.entries!.find((entry) => entry.name === 'wlan0')!
    wlan.lease = { available: true, server: '192.168.30.1', router: '192.168.30.1', remainingSeconds: 42358 }
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
    const state = (await screen.findByRole('heading', { name: 'Status' })).closest('[data-slot=card]') as HTMLElement
    expect(await within(state).findByText('-52 dBm · Strong')).toBeTruthy()
    expect(within(state).getByText('WPA3-SAE')).toBeTruthy()
    expect(within(state).getByText('02:00:00:00:03:01')).toBeTruthy()
    expect(within(state).getByText('72 Mbit/s')).toBeTruthy()
    expect(within(state).getByText('192.168.30.1')).toBeTruthy()
    expect(within(state).getByText('5 GHz · 36')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))
    expect(await screen.findByText('harbour')).toBeTruthy()
    expect(screen.getByText('2.4 GHz · 6 / 5 GHz · 36')).toBeTruthy()
    expect(screen.getByText('-80 dBm · Weak')).toBeTruthy()
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
    const fetch = stubFetch({ ...joined(), 'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'off' }, 202) })
    open('wifi')
    await screen.findByText('-52 dBm · Strong')
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('switch', { name: 'Wi-Fi' }))
    await userEvent.click(within(screen.getByRole('region', { name: /Editing/ })).getByRole('button', { name: 'Save' }))
    const confirm = await screen.findByRole('alertdialog')
    await userEvent.click(within(confirm).getByRole('button', { name: 'Turn off' }))
    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi')).toEqual({ enabled: false, mode: 'client' }))
  })

  it('writes the radio\'s own addressing as an interface', async () => {
    const fetch = stubFetch({ ...joined(), 'PUT /api/v1/wifi': () => jsonResponse({ taskId: 'w' }) })
    open('wifi')
    await screen.findByText('-52 dBm · Strong')
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Static IP address' }))
    await userEvent.type(screen.getByLabelText('Address'), '192.168.30.5/24')
    await userEvent.type(screen.getByLabelText('Gateway'), '10.0.0.1')
    const bar = screen.getByRole('region', { name: /Editing/ })
    expect(await screen.findByText('The gateway is outside this subnet.')).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true)
    await userEvent.clear(screen.getByLabelText('Gateway'))
    await userEvent.type(screen.getByLabelText('Gateway'), '192.168.30.1')
    await userEvent.click(within(bar).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/wifi')).toEqual({ enabled: true, mode: 'client', client: { addressing: { dhcp: false, static: { address: '192.168.30.5/24', gateway: '192.168.30.1', dns: [] } } } }))
  })
})

describe('the Wi-Fi tab as an access point', () => {
  it('lists the connected stations and refuses an unconfirmed country or a long SSID', async () => {
    const status = observed({ accessPoint: { available: true, entries: [{ interface: 'wlan0', stationCount: 1, stations: [{ mac: '02:00:00:00:02:01', signalDbm: -48, connectedSeconds: 600, rxBytes: 2_500_000, txBytes: 1_200 }] }] } })
    status.accessPoint.entries![0] = { ...status.accessPoint.entries![0], channel: 11, frequencyMhz: 2462 }
    stubFetch({ ...networkRoutes(configured(), status), '/api/v1/wifi/ap': accessPoint('always') })
    open('wifi')
    expect(await screen.findByText('02:00:00:00:02:01')).toBeTruthy()
    const state = screen.getByRole('heading', { name: 'Status' }).closest('[data-slot=card]') as HTMLElement
    expect(within(state).getByText('Channel 11 · 2.4 GHz')).toBeTruthy()
    expect(within(state).getByText('WPA2-PSK')).toBeTruthy()
    const config = screen.getByRole('heading', { name: 'Wi-Fi configuration' }).closest('[data-slot=card]') as HTMLElement
    expect(within(config).getAllByRole('heading', { level: 3 }).map((heading) => heading.textContent)).toEqual(['Wi-Fi', 'Start', 'Network', 'Radio', 'Address and DHCP'])
    expect(screen.getByText('10 min')).toBeTruthy()
    expect(screen.getByText('2.5 MB / 1 KB')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
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
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
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
