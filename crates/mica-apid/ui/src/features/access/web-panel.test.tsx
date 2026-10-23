import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { WebPanel, consoleUrl } from './web-panel'
import { ApiGoneGate, resetApiGone } from './api-gone'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  resetApiGone()
})

const defaults = { mode: 'network' as const, httpPort: 8080, httpsEnabled: false, httpsPort: 8443 }

describe('the web console panel', () => {
  it('reads the listeners the device holds', async () => {
    stubFetch({ '/api/v1/web': defaults })
    renderPanel(<WebPanel />)

    expect(await screen.findByDisplayValue('8080')).toBeTruthy()
    expect((screen.getByLabelText(/HTTPS port/) as HTMLInputElement).disabled).toBe(true)
  })

  /// The ports and the switch reach the device as one document, and the page
  /// says where the console went.
  it('writes the whole document and names the new address', async () => {
    const fetch = stubFetch({
      '/api/v1/web': defaults,
      'PUT /api/v1/web': () => jsonResponse({ taskId: 'task-3' }, 202),
    })
    renderPanel(<WebPanel />)

    await screen.findByDisplayValue('8080')
    await userEvent.click(screen.getByRole('switch', { name: 'HTTPS' }))
    const https = screen.getByLabelText(/HTTPS port/)
    await userEvent.clear(https)
    await userEvent.type(https, '9443')
    await userEvent.click(screen.getByRole('button', { name: 'Save and restart the console' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(true))
    const call = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ mode: 'network', httpPort: 8080, httpsEnabled: true, httpsPort: 9443 })
    expect(await screen.findByText(/Open https:\/\/.*:9443\//)).toBeTruthy()
  })

  /// A device may run with no API at all, so no SSH is asked for first.
  it('offers to turn the API off without SSH', async () => {
    stubFetch({ '/api/v1/web': defaults })
    renderPanel(<WebPanel />)

    await screen.findByDisplayValue('8080')
    await userEvent.click(screen.getByRole('button', { name: 'Off' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save and restart the console' }))
    expect(await screen.findByRole('button', { name: 'Leave the network' })).toBeTruthy()
  })

  /// Leaving asks once, then the whole console gives way to the way back.
  it('confirms, writes the mode and leaves the screen that says how to come back', async () => {
    const fetch = stubFetch({
      '/api/v1/web': defaults,
      'PUT /api/v1/web': () => jsonResponse({ taskId: 'task-4' }, 202),
    })
    renderPanel(<ApiGoneGate><WebPanel /></ApiGoneGate>)

    await screen.findByDisplayValue('8080')
    await userEvent.click(screen.getByRole('button', { name: 'Device only' }))
    expect(screen.queryByLabelText(/HTTP port/)).toBeNull()
    await userEvent.click(screen.getByRole('button', { name: 'Save and restart the console' }))
    await userEvent.click(await screen.findByRole('button', { name: 'Leave the network' }))

    expect(await screen.findByText('API access is now device only')).toBeTruthy()
    expect(screen.getByText(/micad web network/)).toBeTruthy()
    const call = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ ...defaults, mode: 'local' })
  })

  it('addresses the console by the listener that serves it', () => {
    expect(consoleUrl(defaults, 'mica.local')).toBe('http://mica.local:8080/')
    expect(consoleUrl({ ...defaults, httpsEnabled: true }, 'mica.local')).toBe('https://mica.local:8443/')
    expect(consoleUrl({ ...defaults, httpPort: 80 }, 'mica.local')).toBe('http://mica.local/')
    expect(consoleUrl({ ...defaults, httpsEnabled: true, httpsPort: 443 }, 'mica.local')).toBe('https://mica.local/')
  })
})
