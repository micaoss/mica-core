import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { WebPanel, consoleUrl } from './web-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const defaults = { httpPort: 8080, httpsEnabled: false, httpsPort: 8443 }

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
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ httpPort: 8080, httpsEnabled: true, httpsPort: 9443 })
    expect(await screen.findByText(/Open https:\/\/.*:9443\//)).toBeTruthy()
  })

  it('addresses the console by the listener that serves it', () => {
    expect(consoleUrl(defaults, 'mica.local')).toBe('http://mica.local:8080/')
    expect(consoleUrl({ ...defaults, httpsEnabled: true }, 'mica.local')).toBe('https://mica.local:8443/')
  })
})
