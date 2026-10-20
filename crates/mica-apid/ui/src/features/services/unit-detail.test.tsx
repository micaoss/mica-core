import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { UnitDetailPage } from './unit-detail'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const ROUTE = '/services_/units/$name'
const script = '#!/sbin/openrc-run\ncommand=/mica/apps/app/run\n'

const routes = {
  '/api/v1/units': {
    init: 'openrc',
    available: true,
    units: [
      { name: 'app', declared: true, managed: true, installed: true, enabled: true, active: 'started', kind: 'openrc' },
      { name: 'old', declared: false, managed: false, installed: true, enabled: false, active: 'stopped' },
    ],
  },
  '/api/v1/settings/services.units': { app: { kind: 'openrc', text: script, conf: 'PORT=80\n', enabled: true } },
  '/api/v1/units/app/logs': { source: 'app', available: true, lines: ['started', 'password=[redacted]'], truncated: false },
  '/api/v1/units/old/logs': { source: 'old', available: false, detail: 'no RAM log on this device' },
}

function renderPage(name: string) {
  return renderParamRoute(<UnitDetailPage />, ROUTE, `/services_/units/${name}`)
}

function sent(fetch: ReturnType<typeof stubFetch>, method: string, path: string) {
  return fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
}

describe('one of the user\'s services', () => {
  it('shows its state, its script with its settings, and its log', async () => {
    stubFetch(routes)
    renderPage('app')

    expect(await screen.findByText('Declared in the settings and kept by micad.')).toBeTruthy()
    expect(screen.getByText('started')).toBeTruthy()
    expect((screen.getByLabelText('Script') as HTMLTextAreaElement).value).toBe(script)
    expect((screen.getByLabelText('Settings (conf.d)') as HTMLTextAreaElement).value).toBe('PORT=80\n')
    expect(await screen.findByText(/password=\[redacted\]/)).toBeTruthy()
  })

  it('saves an edited text with the rest of the service', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/units/app': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderPage('app')

    const text = await screen.findByLabelText('Script')
    await userEvent.type(text, 'pidfile=/run/app.pid')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(sent(fetch, 'PUT', '/api/v1/units/app')).toBeTruthy())
    expect(JSON.parse(String((sent(fetch, 'PUT', '/api/v1/units/app')?.[1] as RequestInit).body))).toEqual({
      kind: 'openrc', text: `${script}pidfile=/run/app.pid`, conf: 'PORT=80\n', enabled: true,
    })
    expect((await screen.findAllByText('Service app saved.')).length).toBeGreaterThan(0)
  })

  it('sends the init\'s verbs', async () => {
    const fetch = stubFetch({ ...routes, 'POST /api/v1/units/app/restart': () => jsonResponse(null, 204) })
    renderPage('app')

    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }))

    expect((await screen.findAllByText('Restarting app.')).length).toBeGreaterThan(0)
    expect(sent(fetch, 'POST', '/api/v1/units/app/restart')).toBeTruthy()
  })

  it('offers no editing for a service installed by hand', async () => {
    stubFetch(routes)
    renderPage('old')

    expect(await screen.findByText(/Installed with mica-unit by hand/)).toBeTruthy()
    expect(screen.queryByLabelText('Script')).toBeNull()
    expect(screen.queryByRole('switch')).toBeNull()
    expect((screen.getByRole('button', { name: 'Start' }) as HTMLButtonElement).disabled).toBe(false)
    expect(await screen.findByText('no RAM log on this device')).toBeTruthy()
  })

  it('names a service that is not there', async () => {
    stubFetch(routes)
    renderPage('nothing')

    expect(await screen.findByText('No service is called nothing.')).toBeTruthy()
  })
})
