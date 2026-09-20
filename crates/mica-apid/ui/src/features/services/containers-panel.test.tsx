import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { ContainersPanel } from './containers-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const overview = {
  enabled: true,
  declared: {
    'node-red': {
      image: 'docker.io/nodered/node-red:4.0.9',
      publish: [{ host: 1880, container: 1880, protocol: 'tcp' }],
      volumes: [{ host: '/mica/apps/node-red', container: '/data', readOnly: false }],
      restart: 'always',
      autoStart: true,
    },
  },
  engine: { available: true, entries: [{ Names: ['node-red'], State: 'running' }] },
  images: { available: true, entries: [] },
}

describe('the containers panel', () => {
  it('shows what is declared beside what the engine reports', async () => {
    stubFetch({ '/api/v1/containers': overview })
    renderPanel(<ContainersPanel />)

    expect(await screen.findByText('node-red')).toBeTruthy()
    expect(screen.getByText('docker.io/nodered/node-red:4.0.9')).toBeTruthy()
    expect(screen.getByText('running')).toBeTruthy()
    expect(screen.getByText('1880:1880')).toBeTruthy()
  })

  /// The engine and the declaration answer different questions, so an engine
  /// that did not answer is said rather than shown as an empty list.
  it('says the engine did not answer instead of reporting nothing running', async () => {
    stubFetch({
      '/api/v1/containers': {
        ...overview,
        engine: { available: false, detail: '/usr/bin/podman is not present on this image' },
      },
    })
    renderPanel(<ContainersPanel />)

    expect(await screen.findByText(/did not answer/)).toBeTruthy()
    expect(screen.getByText('not running')).toBeTruthy()
  })

  it('warns that nothing runs while the runtime is switched off', async () => {
    stubFetch({ '/api/v1/containers': { ...overview, enabled: false } })
    renderPanel(<ContainersPanel />)

    expect(await screen.findByText(/switched off/)).toBeTruthy()
  })

  /// The lifecycle verbs are the device's, and the console sends the verb and
  /// nothing else: what it drives is a unit, which micad decides.
  it('sends a lifecycle verb for the container it names', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'POST /api/v1/containers/node-red/restart': () => jsonResponse(null, 204),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Restart node-red' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input, init]) =>
      String(input) === '/api/v1/containers/node-red/restart'
      && (init as RequestInit | undefined)?.method === 'POST')).toBe(true))
  })

  it('declares a container from the form as the device takes it', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'PUT /api/v1/containers/metrics': () => jsonResponse({ taskId: 'task-1' }, 202),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Declare container' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Name'), 'metrics')
    await userEvent.type(within(dialog).getByLabelText('Image'), 'docker.io/library/busybox:1')
    await userEvent.type(within(dialog).getByLabelText(/Published/), '9100:9100')
    await userEvent.type(within(dialog).getByLabelText('Volumes'), '/mica/apps/metrics:/data')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({
      image: 'docker.io/library/busybox:1',
      command: [],
      environment: {},
      publish: [{ host: 9100, container: 9100, protocol: 'tcp' }],
      volumes: [{ host: '/mica/apps/metrics', container: '/data', readOnly: false }],
      restart: 'no',
      autoStart: true,
    })
  })

  it('removes a declaration behind a confirmation that says the data stays', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'DELETE /api/v1/containers/node-red': () => jsonResponse({ taskId: 'task-2' }, 202),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Remove node-red' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/not deleted/)).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove node-red' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input, init]) =>
      String(input) === '/api/v1/containers/node-red'
      && (init as RequestInit | undefined)?.method === 'DELETE')).toBe(true))
  })
})
