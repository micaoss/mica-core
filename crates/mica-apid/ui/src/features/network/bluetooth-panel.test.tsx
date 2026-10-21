import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { BleProvisioning, BluetoothOverview } from '@/lib/types'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { BluetoothPanel } from './bluetooth-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const overview: BluetoothOverview = {
  enabled: true,
  declared: { 'AA:BB:CC:DD:EE:01': { name: 'phone', trusted: true, blocked: false } },
  adapter: { available: true, address: '11:22:33:44:55:66', name: 'edge-42', powered: true, discoverable: false },
  devices: {
    available: true,
    entries: [{ address: 'AA:BB:CC:DD:EE:02', name: 'speaker', paired: false, trusted: false, blocked: false, connected: false, rssi: -70 }],
  },
  pending: [],
  pairWindow: null,
}

const provisioning: BleProvisioning = {
  state: 'off',
  trigger: null,
  expiresAt: null,
  autoWhenOffline: true,
  verifierReady: true,
  lockedUntil: null,
  wifiLocked: false,
  history: [{ at: 1_792_000_000, trigger: 'boot', outcome: 'wifi-connected' }],
}

const routes = (more: Record<string, unknown> = {}) => ({
  '/api/v1/bluetooth': overview,
  '/api/v1/bluetooth/provisioning': provisioning,
  ...more,
})

const bodyOf = (fetch: ReturnType<typeof stubFetch>, method: string, path: string) => {
  const call = fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
  return call ? JSON.parse(String((call[1] as RequestInit).body)) as Record<string, unknown> : undefined
}

const open = (): BluetoothOverview => ({
  ...overview,
  adapter: { ...overview.adapter, discoverable: true },
  pairWindow: { expiresAt: Math.floor(Date.now() / 1000) + 150 },
  pending: [{ name: 'Pixel 8', mac: 'AA:BB:CC:DD:EE:02', passkey: '042137', since: 0 }],
})

describe('the bluetooth panel', () => {
  /// Nothing to set about discoverability, the name or a code: the adapter
  /// advertises the hostname and opens only for a pairing window.
  it('offers no discoverable switch, name or code, and lists only paired devices', async () => {
    stubFetch(routes())
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText('phone')).toBeTruthy()
    expect(screen.queryByText('speaker')).toBeNull()
    expect(screen.queryByRole('switch', { name: 'Discoverable' })).toBeNull()
    expect(screen.queryByDisplayValue('421137')).toBeNull()
    expect(screen.getByText('edge-42')).toBeTruthy()
    expect(screen.getByText(/cannot normally be discovered/)).toBeTruthy()
    expect(screen.getByText('No pairing requests waiting.')).toBeTruthy()
  })

  it('says the device has no adapter rather than showing an empty list', async () => {
    stubFetch(routes({
      '/api/v1/bluetooth': {
        ...overview,
        adapter: { available: false, detail: 'bluetoothd reports no adapter on this device' },
      },
    }))
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText(/no adapter/)).toBeTruthy()
    expect((screen.getByRole('button', { name: 'Allow new devices' }) as HTMLButtonElement).disabled).toBe(true)
  })

  /// The configuration is one card: edit, change, save.
  it('saves the adapter switch with powered alone', async () => {
    const fetch = stubFetch(routes({ 'PUT /api/v1/bluetooth': () => jsonResponse({ taskId: 't1' }, 202) }))
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('switch', { name: 'Bluetooth' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/bluetooth')).toEqual({ powered: false }))
    expect(bodyOf(fetch, 'PUT', '/api/v1/bluetooth/provisioning/settings')).toBeUndefined()
  })

  /// Turning the offline trigger off says what it costs first.
  it('asks before turning provisioning when offline off', async () => {
    const fetch = stubFetch(routes({ 'PUT /api/v1/bluetooth/provisioning/settings': () => jsonResponse({ taskId: 't2' }, 202) }))
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('switch', { name: 'Provisioning when offline' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/within 10 minutes of boot/)).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(bodyOf(fetch, 'PUT', '/api/v1/bluetooth/provisioning/settings')).toEqual({ autoWhenOffline: false }))
    expect(bodyOf(fetch, 'PUT', '/api/v1/bluetooth')).toBeUndefined()
  })

  /// A manual window: 10 minutes by default, its code in two groups of four,
  /// and a close in place of the open.
  it('opens a manual window and shows its temporary code', async () => {
    const opened: BleProvisioning = { ...provisioning, state: 'waiting', trigger: 'manual', expiresAt: Math.floor(Date.now() / 1000) + 600, code: '20481337' }
    const fetch = stubFetch(routes({
      'POST /api/v1/bluetooth/provisioning': opened,
      'DELETE /api/v1/bluetooth/provisioning': () => jsonResponse(null, 204),
    }))
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText('setup-test.micaos.dev')).toBeTruthy()
    expect(screen.getByText('Wi-Fi connected')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Open provisioning' }))

    await waitFor(() => expect(bodyOf(fetch, 'POST', '/api/v1/bluetooth/provisioning')).toEqual({ durationSeconds: 600 }))
    expect((await screen.findByTestId('provisioning-code')).textContent).toBe('2048 1337')
    expect(screen.getByTestId('provisioning-left').textContent).toMatch(/^(9:5\d|10:00)$/)
    await userEvent.click(screen.getByRole('button', { name: 'Close provisioning' }))
    await waitFor(() => expect(fetch.mock.calls.some(([path, init]) =>
      String(path) === '/api/v1/bluetooth/provisioning' && (init as RequestInit | undefined)?.method === 'DELETE')).toBe(true))
  })

  /// A device claimed before verifiers existed says how to enable recovery,
  /// and a lockout stops the console opening a window.
  it('names a missing verifier and a lockout', async () => {
    stubFetch(routes({ '/api/v1/bluetooth/provisioning': { ...provisioning, verifierReady: false, lockedUntil: 1_900_000_000 } }))
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText(/Sign in to the console once/)).toBeTruthy()
    expect(screen.getByText(/provisioning is locked until/)).toBeTruthy()
    expect((screen.getByRole('button', { name: 'Open provisioning' }) as HTMLButtonElement).disabled).toBe(true)
  })

  /// Trust and block are per device, and say they have nothing to do with
  /// provisioning.
  it('untrusts and blocks a paired device', async () => {
    const fetch = stubFetch(routes({
      'POST /api/v1/bluetooth/devices/AA%3ABB%3ACC%3ADD%3AEE%3A01/untrust': () => jsonResponse(null, 204),
      'POST /api/v1/bluetooth/devices/AA%3ABB%3ACC%3ADD%3AEE%3A01/block': () => jsonResponse(null, 204),
    }))
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText(/provisioning, which needs no pairing/)).toBeTruthy()
    await userEvent.click(await screen.findByRole('button', { name: 'Untrust' }))
    await userEvent.click(screen.getByRole('button', { name: 'Block' }))
    await waitFor(() => {
      const posted = fetch.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === 'POST').map(([path]) => String(path))
      expect(posted).toEqual([
        '/api/v1/bluetooth/devices/AA%3ABB%3ACC%3ADD%3AEE%3A01/untrust',
        '/api/v1/bluetooth/devices/AA%3ABB%3ACC%3ADD%3AEE%3A01/block',
      ])
    })
  })

  it('opens the pairing window on request', async () => {
    const fetch = stubFetch(routes({
      'POST /api/v1/bluetooth/pairing-window': { expiresAt: 1 },
    }))
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Allow new devices' }))

    await waitFor(() => expect(fetch.mock.calls.some(([path, init]) =>
      String(path) === '/api/v1/bluetooth/pairing-window' && (init as RequestInit | undefined)?.method === 'POST')).toBe(true))
  })

  /// While it is open: the time left, a stop, and each request with the
  /// digits the operator compares.
  it('counts the window down and answers a request by its MAC', async () => {
    const fetch = stubFetch(routes({
      '/api/v1/bluetooth': open(),
      'POST /api/v1/bluetooth/pairing-requests/AA%3ABB%3ACC%3ADD%3AEE%3A02': () => jsonResponse(null, 204),
      'DELETE /api/v1/bluetooth/pairing-window': () => jsonResponse(null, 204),
    }))
    renderPanel(<BluetoothPanel />)

    expect((await screen.findByTestId('pair-window')).textContent).toMatch(/^Allowing pairing · 2:(2|3)\d left$/)
    expect(screen.getByText('042137')).toBeTruthy()
    expect(screen.getByText('Pixel 8')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Confirm' }))
    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path]) => String(path).includes('/pairing-requests/'))
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ accept: true })

    await userEvent.click(screen.getByRole('button', { name: 'Stop' }))
    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'DELETE')).toBe(true))
  })

  /// Removing says what it costs: the adapter forgets the keys.
  it('removes a paired device behind a confirmation', async () => {
    const fetch = stubFetch(routes({
      'DELETE /api/v1/bluetooth/devices/AA:BB:CC:DD:EE:01': () => jsonResponse(null, 204),
    }))
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Remove AA:BB:CC:DD:EE:01' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/forgets its keys/)).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove AA:BB:CC:DD:EE:01' }))

    await waitFor(() => expect(fetch.mock.calls.some(([path, init]) =>
      String(path) === '/api/v1/bluetooth/devices/AA%3ABB%3ACC%3ADD%3AEE%3A01'
      && (init as RequestInit | undefined)?.method === 'DELETE')).toBe(true))
  })
})
