import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { MqttPanel } from './mqtt-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const loopback = {
  configured: { enabled: true, listen: { address: '127.0.0.1', port: 1883 }, auth: { enabled: false } },
  observed: {
    available: true,
    state: {
      configPath: '/run/mica/mqtt-broker.toml',
      listen: { address: '127.0.0.1', port: 1883 },
      auth: { enabled: false },
      units: [
        { unit: 'mica-mqtt-broker.service', activeState: 'active' },
        { unit: 'mica-mqttd.service', activeState: 'inactive' },
      ],
    },
  },
}

describe('the MQTT panel', () => {
  it('reads the declared listener and what the broker is bound to', async () => {
    stubFetch({ '/api/v1/mqtt': loopback })
    renderPanel(<MqttPanel />)

    // The form is seeded from the device's own answer, so the wait is for the
    // value and not merely for the field.
    expect(await screen.findByDisplayValue('127.0.0.1')).toBeTruthy()
    expect((screen.getByLabelText(/Port/) as HTMLInputElement).value).toBe('1883')
    expect(screen.getByText('127.0.0.1:1883')).toBeTruthy()
    expect(screen.getByText('/run/mica/mqtt-broker.toml')).toBeTruthy()
  })

  /// The listener is one decision: address, port and the auth flag reach the
  /// device as one document, and `enabled` is carried through untouched so a
  /// listener edit cannot start or stop the broker.
  it('writes the whole document and leaves the switch alone', async () => {
    const fetch = stubFetch({
      '/api/v1/mqtt': loopback,
      'PUT /api/v1/mqtt': () => jsonResponse({ taskId: 'task-9' }, 202),
    })
    renderPanel(<MqttPanel />)

    const address = await screen.findByDisplayValue('127.0.0.1')
    await userEvent.clear(address)
    await userEvent.type(address, '0.0.0.0')
    await userEvent.click(screen.getByRole('switch', { name: 'Require authentication' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save listener' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/mqtt' )).toBe(true))
    const call = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({
      enabled: true,
      listen: { address: '0.0.0.0', port: 1883 },
      auth: { enabled: true },
    })
  })

  /// The warning means something only if it is rare: it fires on a broker that
  /// is running, bound off loopback and taking unauthenticated clients.
  it('warns only about an open listener that accepts anyone', async () => {
    stubFetch({ '/api/v1/mqtt': loopback })
    const quiet = renderPanel(<MqttPanel />)
    expect(await screen.findByDisplayValue('127.0.0.1')).toBeTruthy()
    expect(screen.queryByText(/accepts unauthenticated connections/)).toBeNull()
    quiet.unmount()

    stubFetch({
      '/api/v1/mqtt': {
        ...loopback,
        configured: { enabled: true, listen: { address: '0.0.0.0', port: 1883 }, auth: { enabled: false } },
      },
    })
    renderPanel(<MqttPanel />)
    expect(await screen.findByText(/accepts unauthenticated connections/)).toBeTruthy()
  })

  it('says the reconciler published nothing rather than showing a default broker', async () => {
    stubFetch({
      '/api/v1/mqtt': {
        configured: { enabled: false, listen: { address: '127.0.0.1', port: 1883 }, auth: { enabled: false } },
        observed: { available: false, error: 'the MQTT reconciler has published no state' },
      },
    })
    renderPanel(<MqttPanel />)

    expect(await screen.findByText(/published no state/)).toBeTruthy()
  })
})
