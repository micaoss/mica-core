import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { AppDetailPage } from './app-detail'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const ROUTE = '/services_/apps/$name'

const demo = {
  name: 'tailscale',
  version: '1.78.1',
  description: 'Tailscale node agent and CLI',
  arch: ['arm64'],
  sha256: 'ab'.repeat(32),
  source: 'seed',
  installedAt: '2026-10-21T08:00:00Z',
  previous: '1.76.6',
  units: [{ name: 'tailscaled.service', enabled: true, active: 'active' }],
  last: { action: 'upgrade', outcome: 'ok', at: '2026-10-21T08:00:00Z' },
  update: { url: 'https://apps.example/tailscale/latest.json', auto: false, available: '1.80.0', checkedAt: '2026-10-21T08:00:00Z', error: null },
  seed: { version: '1.78.1', policy: 'follow-image' },
}

const task = { id: 'task-1', operation: 'app.update', kind: 'app.apply', dotPath: 'apps.tailscale', source: ':1.7', status: 'finished', phase: 'applied', outcome: 'succeeded', enqueuedAt: '2026-10-21T08:00:00Z', foldedCount: 0 }

const routes = {
  '/api/v1/apps': { available: true, arch: 'arm64', apps: [demo] },
  '/api/v1/apps/tailscale/logs': {
    name: 'tailscale',
    units: [{ unit: 'tailscaled.service', source: 'tailscaled.service', available: true, lines: ['wgengine: started', 'authkey=[redacted]'] }],
    firstRun: ['state directory made'],
  },
  '/api/v1/tasks/task-1': task,
}

function renderPage(name = 'tailscale') {
  return renderParamRoute(<AppDetailPage />, ROUTE, `/services_/apps/${name}`)
}

function sent(fetch: ReturnType<typeof stubFetch>, method: string, path: string) {
  return fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
}

describe('one app', () => {
  it('shows its version, where it came from, its units, its update and its log', async () => {
    stubFetch(routes)
    renderPage()

    expect(await screen.findByText('Tailscale node agent and CLI')).toBeTruthy()
    expect(screen.getByText('1.76.6')).toBeTruthy()
    expect(screen.getByText('the image')).toBeTruthy()
    expect(screen.getByText('tailscaled.service')).toBeTruthy()
    expect(screen.getByText('1.80.0 available')).toBeTruthy()
    expect(await screen.findByText(/authkey=\[redacted\]/)).toBeTruthy()
    expect(screen.getByText('state directory made')).toBeTruthy()
  })

  it('updates, and follows the task micad answers with', async () => {
    const fetch = stubFetch({ ...routes, 'POST /api/v1/apps/tailscale/update': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderPage()

    await userEvent.click(await screen.findByRole('button', { name: 'Update to 1.80.0' }))

    await waitFor(() => expect(sent(fetch, 'POST', '/api/v1/apps/tailscale/update')).toBeTruthy())
    expect(await screen.findByText('Change applied.')).toBeTruthy()
  })

  it('rolls back and removes only once confirmed, and drives its units', async () => {
    const fetch = stubFetch({
      ...routes,
      'POST /api/v1/apps/tailscale/rollback': () => jsonResponse({ taskId: 'task-1' }, 202),
      'POST /api/v1/apps/tailscale/restart': () => new Response(null, { status: 204 }),
      'DELETE /api/v1/apps/tailscale?purge=true': () => jsonResponse({ taskId: 'task-2' }, 202),
    })
    renderPage()

    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }))
    await waitFor(() => expect(sent(fetch, 'POST', '/api/v1/apps/tailscale/restart')).toBeTruthy())

    await userEvent.click(screen.getByRole('button', { name: 'Roll back to 1.76.6' }))
    expect(sent(fetch, 'POST', '/api/v1/apps/tailscale/rollback')).toBeUndefined()
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Roll back to 1.76.6' }))
    await waitFor(() => expect(sent(fetch, 'POST', '/api/v1/apps/tailscale/rollback')).toBeTruthy())

    await userEvent.click(screen.getByRole('button', { name: 'Remove with its data' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Remove with its data' }))
    await waitFor(() => expect(sent(fetch, 'DELETE', '/api/v1/apps/tailscale?purge=true')).toBeTruthy())
  })

  it('says when no such app is installed', async () => {
    stubFetch(routes)
    renderPage('other')

    expect(await screen.findByText(/other/, { selector: 'p' })).toBeTruthy()
  })
})
