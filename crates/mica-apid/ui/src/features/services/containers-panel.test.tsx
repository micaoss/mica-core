import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { ContainersPanel } from './containers-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const compose = 'services:\n  web:\n    image: docker.io/library/nginx:1\n'

const overview = {
  enabled: true,
  projects: { app: compose },
  engine: {
    available: true,
    projects: [{ name: 'app', services: [
      { project: 'app', service: 'web', spec: { name: 'app-web-1', image: 'docker.io/library/nginx:1' }, phase: 'running', health: 'healthy', restarts: 1, exit_code: 0 },
    ] }],
  },
  images: { available: true, entries: [] },
}

function sent(fetch: ReturnType<typeof stubFetch>, method: string, path: string) {
  return fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)
}

describe('the containers panel', () => {
  /// On docker nothing is declared: what dockerd holds is listed with its
  /// verbs, and there is nothing to add, edit or remove.
  it('lists what dockerd holds on a docker product, with its verbs and no editing', async () => {
    stubFetch({ '/api/v1/containers': { ...overview, containerEngine: 'docker', projects: {} } })
    renderPanel(<ContainersPanel />)

    const project = await screen.findByRole('region', { name: 'app' })
    expect(within(project).getByText('running · healthy')).toBeTruthy()
    expect(within(project).queryByText('being removed')).toBeNull()
    expect(within(project).queryByRole('button', { name: 'Edit' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Add project' })).toBeNull()
    expect(screen.getByText(/This is a Docker product/)).toBeTruthy()
    for (const button of within(project).getAllByRole('button', { name: /Stop/ })) {
      expect((button as HTMLButtonElement).disabled).toBe(false)
    }
  })

  it('shows each project with what the engine reports for its services', async () => {
    stubFetch({ '/api/v1/containers': overview })
    renderPanel(<ContainersPanel />)

    const project = await screen.findByRole('region', { name: 'app' })
    expect(within(project).getByText('web')).toBeTruthy()
    expect(within(project).getByText('docker.io/library/nginx:1')).toBeTruthy()
    expect(within(project).getByText('running · healthy')).toBeTruthy()
    expect(within(project).getByText('1 restarts')).toBeTruthy()
  })

  it('shows the project directory', async () => {
    stubFetch({ '/api/v1/containers': { ...overview, projectDirs: { app: '/srv/app' } } })
    renderPanel(<ContainersPanel />)

    const project = await screen.findByRole('region', { name: 'app' })
    expect(within(project).getByText('/srv/app')).toBeTruthy()
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
    expect(screen.getByText('mica-containerd has not taken this project yet.')).toBeTruthy()
  })

  /// Off is the default, and mica-containerd does not run then: its absent
  /// socket is the off state, not an engine that failed to answer.
  it('says containers are off and how to turn them on, not that the engine did not answer', async () => {
    stubFetch({
      '/api/v1/containers': {
        ...overview,
        enabled: false,
        engine: { available: false, detail: 'connect to /run/mica-containerd/api.sock: No such file or directory' },
      },
    })
    renderPanel(<ContainersPanel />)

    expect(await screen.findByText(/Containers are off\. Turn on the switch at the top of this page/)).toBeTruthy()
    expect(screen.queryByText(/did not answer/)).toBeNull()
    expect(screen.queryByText(/api\.sock/)).toBeNull()
  })

  it('sends a verb to one service, or to the whole project', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'POST /api/v1/container-projects/app/services/web/restart': () => jsonResponse(null, 204),
      'POST /api/v1/container-projects/app/stop': () => jsonResponse(null, 204),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Restart app/web' }))
    await userEvent.click(screen.getByRole('button', { name: 'Stop app' }))

    await waitFor(() => expect(sent(fetch, 'POST', '/api/v1/container-projects/app/services/web/restart')).toBeTruthy())
    await waitFor(() => expect(sent(fetch, 'POST', '/api/v1/container-projects/app/stop')).toBeTruthy())
  })

  it('shows a service log', async () => {
    stubFetch({
      '/api/v1/containers': overview,
      '/api/v1/container-projects/app/services/web/logs?tail=200': { log: 'listening on :80\n' },
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Log of app/web' }))
    const dialog = await screen.findByRole('dialog')
    expect(await within(dialog).findByText(/listening on :80/)).toBeTruthy()
  })

  /// The check is mica-containerd's, run on the device: the console shows
  /// every problem by service and field, and saves nothing.
  it('checks a new project and shows every problem by service and field', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'POST /api/v1/container-projects/metrics/check': {
        ok: false,
        detail: 'web: privileged is not supported',
        problems: [{ service: 'web', field: 'privileged', detail: 'privileged is not supported' }],
      },
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add project' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Name'), 'metrics')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Check' }))

    expect(await within(dialog).findByText('web.privileged')).toBeTruthy()
    expect(within(dialog).getByText(/privileged is not supported/)).toBeTruthy()
    const call = sent(fetch, 'POST', '/api/v1/container-projects/metrics/check')
    expect(JSON.parse(String((call?.[1] as RequestInit).body)).compose).toContain('services:')
    expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(false)
  })

  it('saves the Compose text as the device takes it', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'PUT /api/v1/container-projects/app': () => jsonResponse({ taskId: 'task-1' }, 202),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    const text = within(dialog).getByLabelText(/Compose file/) as HTMLTextAreaElement
    expect(text.value).toBe(compose)
    await userEvent.type(text, '    restart: always\n')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => expect(sent(fetch, 'PUT', '/api/v1/container-projects/app')).toBeTruthy())
    const call = sent(fetch, 'PUT', '/api/v1/container-projects/app')
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ compose: `${compose}    restart: always\n` })
  })

  /// A refused save keeps the dialog and the text, with the problems beside it.
  it('keeps the editor open with the problems of a refused save', async () => {
    stubFetch({
      '/api/v1/containers': overview,
      'PUT /api/v1/container-projects/app': () => jsonResponse({ error: {
        code: 'compose_invalid',
        message: 'container project "app" is refused: web: build is not supported',
        problems: [{ service: 'web', field: 'build', detail: 'build is not supported' }],
      } }, 422),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(await within(dialog).findByText('web.build')).toBeTruthy()
    expect(dialog.isConnected).toBe(true)
  })

  it('removes a project behind a confirmation that says the data stays', async () => {
    const fetch = stubFetch({
      '/api/v1/containers': overview,
      'DELETE /api/v1/container-projects/app': () => jsonResponse({ taskId: 'task-2' }, 202),
    })
    renderPanel(<ContainersPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Remove app' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/not deleted/)).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove app' }))

    await waitFor(() => expect(sent(fetch, 'DELETE', '/api/v1/container-projects/app')).toBeTruthy())
  })
})
