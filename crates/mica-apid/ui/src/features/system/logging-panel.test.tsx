import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderPanel, stubFetch } from '@/shared/testing/panel'
import { LoggingPanel } from './logging-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const SETTINGS = { remote: { enabled: false, address: '', port: 514, protocol: 'udp' } }
const STATE = { supported: true, persistent: false, persistentSource: 'default', maxSizeKiB: 4096, maxSizeKiBLimit: 8192 }

describe('the logging panel', () => {
  it('shows the product default, the device bound and the fixed warning', async () => {
    stubFetch({
      '/api/v1/meta': { features: [], init: 'systemd' },
      '/api/v1/settings/logging': SETTINGS,
      '/api/v1/state/logging': STATE,
    })
    renderPanel(<LoggingPanel />)
    expect(await screen.findByText('Product default (in memory)')).toBeTruthy()
    expect(await screen.findByText(/at most 8192 KiB on this device/)).toBeTruthy()
    expect(screen.getByText('Logs are sent unencrypted and unredacted. Use a collector reached through WireGuard/VPN only.')).toBeTruthy()

    await userEvent.click(screen.getByRole('combobox', { name: 'Protocol' }))
    expect(await screen.findByRole('option', { name: 'TCP' })).toBeTruthy()
  })

  it('offers UDP alone on an OpenRC device', async () => {
    stubFetch({
      '/api/v1/meta': { features: [], init: 'openrc' },
      '/api/v1/settings/logging': SETTINGS,
      '/api/v1/state/logging': STATE,
    })
    renderPanel(<LoggingPanel />)
    expect(await screen.findByText('This device forwards over UDP only.')).toBeTruthy()
    await userEvent.click(screen.getByRole('combobox', { name: 'Protocol' }))
    expect(await screen.findByRole('option', { name: 'UDP' })).toBeTruthy()
    expect(screen.queryByRole('option', { name: 'TCP' })).toBeNull()
  })

  it('writes the three settings in one request, unset values as null', async () => {
    const fetch = stubFetch({
      '/api/v1/meta': { features: [], init: 'systemd' },
      '/api/v1/settings/logging': SETTINGS,
      '/api/v1/state/logging': STATE,
      'PATCH /api/v1/settings': { taskId: 't1' },
      '/api/v1/tasks/t1': { id: 't1', status: 'finished', outcome: 'succeeded' },
    })
    renderPanel(<LoggingPanel />)
    await userEvent.click(await screen.findByRole('switch', { name: 'Forward the log' }))
    await userEvent.type(screen.getByLabelText('Collector address'), '192.0.2.10')
    await userEvent.click(screen.getByRole('button', { name: 'Save logging' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PATCH')).toBe(true))
    const [, init] = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PATCH')!
    expect(JSON.parse(String((init as RequestInit).body))).toEqual({
      'logging.persistent': null,
      'logging.maxSizeKiB': null,
      'logging.remote': { enabled: true, address: '192.0.2.10', port: 514, protocol: 'udp' },
    })
  })
})
