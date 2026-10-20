import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderRoute, stubFetch } from '@/shared/testing/panel'
import { UnitsPanel } from './units-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const unit = '[Unit]\nDescription=app\n\n[Service]\nExecStart=/mica/apps/app/run\n\n[Install]\nWantedBy=multi-user.target\n'

const routes = {
  '/api/v1/units': {
    init: 'systemd',
    available: true,
    units: [
      { name: 'app.service', declared: true, managed: true, installed: true, enabled: true, active: 'active', kind: 'systemd' },
      { name: 'old.service', declared: false, managed: false, installed: true, enabled: false, active: 'inactive' },
    ],
  },
  '/api/v1/settings/services.units': { 'app.service': { kind: 'systemd', text: unit, enabled: true } },
}

function sent(fetch: ReturnType<typeof stubFetch>, method: string, path: string) {
  const call = fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
  return call ? JSON.parse(String((call[1] as RequestInit).body)) as unknown : undefined
}

describe('the user\'s services', () => {
  it('lists every service with its state, and a switch only for a declared one', async () => {
    stubFetch(routes)
    renderRoute(<UnitsPanel />)

    const app = (await screen.findByRole('link', { name: 'app.service' })).closest('tr') as HTMLElement
    expect(within(app).getByText('active')).toBeTruthy()
    expect(within(app).getByRole('switch', { name: 'app.service' })).toBeTruthy()
    const old = screen.getByRole('link', { name: 'old.service' }).closest('tr') as HTMLElement
    expect(within(old).getByText('installed by hand')).toBeTruthy()
    expect(within(old).queryByRole('switch')).toBeNull()
  })

  it('writes the whole service when its switch turns', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/units/app.service': () => jsonResponse({ taskId: 'task-1' }, 202) })
    renderRoute(<UnitsPanel />)

    await userEvent.click(await screen.findByRole('switch', { name: 'app.service' }))

    expect((await screen.findAllByText('app.service disabled.')).length).toBeGreaterThan(0)
    expect(sent(fetch, 'PUT', '/api/v1/units/app.service')).toEqual({ kind: 'systemd', text: unit, enabled: false })
  })

  it('adds a service of the device\'s init from a template', async () => {
    const fetch = stubFetch({ ...routes, 'PUT /api/v1/units/new.service': () => jsonResponse({ taskId: 'task-2' }, 202) })
    renderRoute(<UnitsPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add service' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).queryByLabelText('Settings (conf.d)')).toBeNull()
    await userEvent.type(within(dialog).getByLabelText('Name'), 'new.service')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(sent(fetch, 'PUT', '/api/v1/units/new.service')).toMatchObject({ kind: 'systemd', enabled: true }))
    const body = sent(fetch, 'PUT', '/api/v1/units/new.service') as { text: string }
    expect(body.text).toContain('WantedBy=multi-user.target')
  })

  it('offers the script\'s settings on OpenRC', async () => {
    stubFetch({ ...routes, '/api/v1/units': { init: 'openrc', available: true, units: [] } })
    renderRoute(<UnitsPanel />)

    expect(await screen.findByText('No services of your own are on this device.')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Add service' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).getByLabelText('Settings (conf.d)')).toBeTruthy()
    expect((within(dialog).getByLabelText('Script') as HTMLTextAreaElement).value).toMatch(/^#!\/sbin\/openrc-run/)
  })

  it('says so and offers nothing to add without mica-unit', async () => {
    stubFetch({ ...routes, '/api/v1/units': { init: 'systemd', available: false, units: [] } })
    renderRoute(<UnitsPanel />)

    expect(await screen.findByText('This device has no mica-unit, so no service can be declared.')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Add service' })).toBeNull()
  })
})
