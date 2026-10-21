import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderRoute, stubFetch } from '@/shared/testing/panel'
import { AppsPanel } from './apps-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const demo = {
  name: 'tailscale',
  version: '1.78.1',
  description: 'Tailscale node agent and CLI',
  arch: ['arm64'],
  sha256: 'ab'.repeat(32),
  source: 'upload',
  installedAt: '2026-10-21T08:00:00Z',
  previous: '1.76.6',
  units: [{ name: 'tailscaled.service', enabled: true, active: 'active' }],
  privileges: { user: 'app', account: 'mica-app-tailscale', uid: 30000, capabilities: ['CAP_NET_ADMIN', 'CAP_NET_RAW'], devices: ['/dev/net/tun'] },
  last: { action: 'upgrade', outcome: 'ok', at: '2026-10-21T08:00:00Z' },
  update: { url: 'https://apps.example/tailscale/latest.json', auto: false, available: '1.80.0', checkedAt: '2026-10-21T08:00:00Z', error: null },
  seed: null,
}

const routes = {
  '/api/v1/apps': { available: true, arch: 'arm64', apps: [demo] },
  '/api/v1/tasks/task-1': { id: 'task-1', operation: 'app.install', kind: 'app.apply', dotPath: 'apps.tailscale', source: ':1.7', status: 'running', phase: 'applying', enqueuedAt: '2026-10-21T08:00:00Z', foldedCount: 0 },
}

async function upload(name = 'tailscale-1.80.0-arm64.tar.gz') {
  const bundle = new File([new Uint8Array([0x1f, 0x8b])], name)
  await userEvent.upload(document.querySelector('input[type="file"]') as HTMLInputElement, bundle)
  await userEvent.click(screen.getByRole('button', { name: 'Install' }))
  return bundle
}

describe('the apps', () => {
  it('lists each app with its version, an update offered and its state', async () => {
    stubFetch(routes)
    renderRoute(<AppsPanel />)

    const row = (await screen.findByRole('link', { name: 'tailscale' })).closest('tr') as HTMLElement
    expect(within(row).getByText('1.78.1')).toBeTruthy()
    expect(within(row).getByText('1.80.0 available')).toBeTruthy()
    expect(within(row).getByText('running')).toBeTruthy()
    expect(within(row).queryByText('runs as root')).toBeNull()
  })

  it('marks an app that runs as root', async () => {
    const root = { ...demo, name: 'agent', privileges: { user: 'root', account: null, uid: null, capabilities: [], devices: [] } }
    stubFetch({ ...routes, '/api/v1/apps': { available: true, arch: 'arm64', apps: [root] } })
    renderRoute(<AppsPanel />)

    const row = (await screen.findByRole('link', { name: 'agent' })).closest('tr') as HTMLElement
    expect(within(row).getByText('runs as root')).toBeTruthy()
  })

  it('uploads a bundle as the body of one request and follows its task', async () => {
    const fetch = stubFetch({ ...routes, 'POST /api/v1/apps': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderRoute(<AppsPanel />)
    await screen.findByRole('link', { name: 'tailscale' })

    const bundle = await upload()

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path, init]) => String(path) === '/api/v1/apps' && init?.method === 'POST')
      expect(found).toBeTruthy()
      return found
    })
    const init = call?.[1] as RequestInit
    expect(new Headers(init.headers).get('content-type')).toBe('application/octet-stream')
    expect(init.body).toBe(bundle)
    expect(await screen.findByText('Applying apps.tailscale…')).toBeTruthy()
  })

  it('names every rule a refused bundle breaks', async () => {
    stubFetch({
      ...routes,
      'POST /api/v1/apps': () => jsonResponse({ error: {
        code: 'app_invalid',
        message: 'the bundle is refused',
        source: 'micad',
        rules: [
          { rule: 'setuid', detail: 'files/su is setuid or setgid' },
          { rule: 'unit_taken', detail: 'tailscaled.service is a service in services.units' },
        ],
      } }, 422),
    })
    renderRoute(<AppsPanel />)
    await screen.findByRole('link', { name: 'tailscale' })

    await upload()

    expect(await screen.findByText('The bundle is refused:')).toBeTruthy()
    expect(screen.getByText(': files/su is setuid or setgid', { exact: false })).toBeTruthy()
    expect(screen.getByText('unit_taken')).toBeTruthy()
  })

  it('asks before it installs a bundle that asks for more privileges', async () => {
    const fetch = stubFetch({
      ...routes,
      'POST /api/v1/apps': () => jsonResponse({ error: {
        code: 'app_privileges',
        message: 'tailscale 1.80.0 asks for privileges beyond 1.78.1; accept them to install it',
        source: 'micad',
        params: { root: false, capabilities: ['CAP_NET_BIND_SERVICE'], devices: ['/dev/ttyUSB0'] },
      } }, 409),
      'POST /api/v1/apps?acceptPrivileges=true': () => jsonResponse({ taskId: 'task-1' }, 202),
    })
    renderRoute(<AppsPanel />)
    await screen.findByRole('link', { name: 'tailscale' })

    const bundle = await upload()

    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('The bundle asks for more privileges')).toBeTruthy()
    expect(within(dialog).getByText('Capabilities: CAP_NET_BIND_SERVICE')).toBeTruthy()
    expect(within(dialog).getByText('Devices: /dev/ttyUSB0')).toBeTruthy()
    expect(within(dialog).queryByText(/runs as root/)).toBeNull()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Install with these privileges' }))

    expect(await screen.findByText('Applying apps.tailscale…')).toBeTruthy()
    const accepted = fetch.mock.calls.find(([path]) => String(path) === '/api/v1/apps?acceptPrivileges=true')
    expect((accepted?.[1] as RequestInit).body).toBe(bundle)
  })

  it('installs nothing when the privileges are declined', async () => {
    const fetch = stubFetch({
      ...routes,
      'POST /api/v1/apps': () => jsonResponse({ error: {
        code: 'app_privileges', message: 'agent 1.0 asks for privileges', source: 'micad',
        params: { root: true, capabilities: [], devices: [] },
      } }, 409),
    })
    renderRoute(<AppsPanel />)
    await screen.findByRole('link', { name: 'tailscale' })

    await upload()

    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('It runs as root, with full control of the device.')).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))

    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(fetch.mock.calls.some(([path]) => String(path).includes('acceptPrivileges'))).toBe(false)
  })

  it('says so on a device that keeps no apps', async () => {
    stubFetch({ ...routes, '/api/v1/apps': { available: false, arch: 'arm64', apps: [] } })
    renderRoute(<AppsPanel />)

    expect(await screen.findByText('This device keeps no apps.')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Install' })).toBeNull()
  })
})
