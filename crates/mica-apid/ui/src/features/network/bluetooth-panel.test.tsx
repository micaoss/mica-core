import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { BluetoothOverview } from '@/lib/types'
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
    stubFetch({ '/api/v1/bluetooth': overview })
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
    stubFetch({
      '/api/v1/bluetooth': {
        ...overview,
        adapter: { available: false, detail: 'bluetoothd reports no adapter on this device' },
      },
    })
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText(/no adapter/)).toBeTruthy()
    expect((screen.getByRole('button', { name: 'Allow new devices' }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('switches the adapter with powered alone', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': overview,
      'PUT /api/v1/bluetooth': () => jsonResponse({ taskId: 't1' }, 202),
    })
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('switch', { name: 'Bluetooth' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ powered: false })
  })

  it('opens the pairing window on request', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': overview,
      'POST /api/v1/bluetooth/pairing-window': { expiresAt: 1 },
    })
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Allow new devices' }))

    await waitFor(() => expect(fetch.mock.calls.some(([path, init]) =>
      String(path) === '/api/v1/bluetooth/pairing-window' && (init as RequestInit | undefined)?.method === 'POST')).toBe(true))
  })

  /// While it is open: the time left, a stop, and each request with the
  /// digits the operator compares.
  it('counts the window down and answers a request by its MAC', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': open(),
      'POST /api/v1/bluetooth/pairing-requests/AA%3ABB%3ACC%3ADD%3AEE%3A02': () => jsonResponse(null, 204),
      'DELETE /api/v1/bluetooth/pairing-window': () => jsonResponse(null, 204),
    })
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
    const fetch = stubFetch({
      '/api/v1/bluetooth': overview,
      'DELETE /api/v1/bluetooth/devices/AA:BB:CC:DD:EE:01': () => jsonResponse(null, 204),
    })
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
