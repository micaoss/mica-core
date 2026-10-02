import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { BluetoothPanel } from './bluetooth-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const overview = {
  enabled: true,
  discoverable: false,
  pin: '4211',
  declared: { 'AA:BB:CC:DD:EE:01': { name: 'phone', trusted: true, blocked: false } },
  adapter: { available: true, address: '11:22:33:44:55:66', alias: 'edge-42', powered: true, discoverable: false, discovering: false },
  devices: {
    available: true,
    entries: [{ address: 'AA:BB:CC:DD:EE:02', name: 'speaker', paired: false, trusted: false, blocked: false, connected: false, rssi: -70 }],
  },
  pending: null,
}

describe('the bluetooth panel', () => {
  /// The code is shown, not masked: somebody has to type it on the other
  /// device, which is the whole of what it is for.
  it('shows the pairing code rather than hiding it', async () => {
    stubFetch({ '/api/v1/bluetooth': overview })
    renderPanel(<BluetoothPanel />)

    // The field is seeded from the device's answer, so the wait is for the
    // value rather than for the field.
    const pin = await screen.findByDisplayValue('4211') as HTMLInputElement
    expect(pin.getAttribute('type')).not.toBe('password')
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
  })

  /// The dialog is the one thing here that has to appear without a refresh,
  /// and it carries the passkey the operator compares.
  it('asks for a decision when a pairing is waiting, and answers it', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': { ...overview, pending: { address: 'AA:BB:CC:DD:EE:02', passkey: 123_456, since: 0 } },
      'POST /api/v1/bluetooth/devices/AA:BB:CC:DD:EE:02/confirm': () => jsonResponse(null, 204),
    })
    renderPanel(<BluetoothPanel />)

    expect(await screen.findByText('123456')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Codes match' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path]) => String(path).endsWith('/confirm'))
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ accept: true })
  })

  it('pairs with a device the adapter found', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': overview,
      'POST /api/v1/bluetooth/devices/AA:BB:CC:DD:EE:02/pair': () => jsonResponse(null, 204),
    })
    renderPanel(<BluetoothPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Pair' }))

    await waitFor(() => expect(fetch.mock.calls.some(([path]) => String(path).endsWith('/pair'))).toBe(true))
  })

  /// Removing says what it costs: the adapter forgets the keys.
  it('removes a trusted device behind a confirmation', async () => {
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

  it('starts a scan on request and never on a render', async () => {
    const fetch = stubFetch({
      '/api/v1/bluetooth': overview,
      'POST /api/v1/bluetooth/discovery': () => jsonResponse(null, 204),
    })
    renderPanel(<BluetoothPanel />)

    await screen.findByText('speaker')
    expect(fetch.mock.calls.some(([path]) => String(path).endsWith('/discovery'))).toBe(false)

    await userEvent.click(screen.getByRole('button', { name: 'Scan' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path]) => String(path).endsWith('/discovery'))
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ on: true })
  })
})
