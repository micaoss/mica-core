import { act, cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { ManualUpdate, UpdateOverview, UpdatePanel } from './system-page'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('the update overview', () => {
  it('installs the newest release in one confirmed request', async () => {
    const fetch = stubFetch({
      '/api/v1/update': { lifecycle: { state: 'idle', available: { deploymentId: 'a'.repeat(64), version: '0.0.6', kind: 'set', setKind: 'core' } } },
      'POST /api/v1/update/install-latest': () => new Response(null, { status: 202 }),
    })
    renderPanel(<UpdateOverview />)
    expect(await screen.findByText('Update available: core 0.0.6')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Update now' }))
    await userEvent.click(screen.getAllByRole('button', { name: 'Update now' }).at(-1)!)
    await waitFor(() => expect(fetch.mock.calls.some(([path]) => path === '/api/v1/update/install-latest')).toBe(true))
    const request = fetch.mock.calls.find(([path]) => path === '/api/v1/update/install-latest')?.[1]
    expect(request?.method).toBe('POST')
    expect(request?.body).toBeUndefined()
  })

  it('offers no install while the device is up to date', async () => {
    stubFetch({ '/api/v1/update': { lifecycle: { state: 'idle', last_check: '2026-10-15T20:00:00Z' } } })
    renderPanel(<UpdateOverview />)
    expect(await screen.findByText('This device is up to date.')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Update now' })).toBeNull()
    expect(screen.getByRole('button', { name: 'Check for updates' }).hasAttribute('disabled')).toBe(false)
  })

  it('follows an install through the gap between its steps to the reboot it owes', async () => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] })
    const offered = { deploymentId: 'a'.repeat(64), version: '0.0.6', kind: 'set', setKind: 'core' }
    const states = [
      { lifecycle: { state: 'idle', available: offered } },
      // The check settled and the download has not started.
      { lifecycle: { state: 'idle', available: offered } },
      { lifecycle: { state: 'downloading', available: offered } },
      { lifecycle: { state: 'reboot-required', available: offered },
        sets: { core: { pending: { id: '2'.repeat(64), channel: 'general', generation: 5, version: '0.0.6' }, boot: { coreSetId: '1'.repeat(64) } } } },
    ]
    let read = 0
    const fetch = vi.fn((path: string) => path === '/api/v1/update/install-latest'
      ? Promise.resolve(new Response(null, { status: 202 }))
      : Promise.resolve(jsonResponse(states[Math.min(read++, states.length - 1)])))
    vi.stubGlobal('fetch', fetch)
    renderPanel(<UpdateOverview />)
    await userEvent.click(await screen.findByRole('button', { name: 'Update now' }))
    await userEvent.click(screen.getAllByRole('button', { name: 'Update now' }).at(-1)!)
    await screen.findByText('Checking for updates')
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    await screen.findByText('Downloading the update')
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    await screen.findByText('The update is installed. Reboot to start it.')
    expect(screen.getByRole('button', { name: 'Reboot now' })).toBeTruthy()
    const calls = fetch.mock.calls.length
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(fetch).toHaveBeenCalledTimes(calls)
  })

  it('names why the last update did not complete', async () => {
    stubFetch({ '/api/v1/update': { lifecycle: { state: 'failed', reason: 'set-check was killed by signal 6 (SIGABRT)' } } })
    renderPanel(<UpdateOverview />)
    expect(await screen.findByText('The last update did not complete.')).toBeTruthy()
    expect(screen.getByText('set-check was killed by signal 6 (SIGABRT)')).toBeTruthy()
  })
})

it('displays the running, candidate and fallback identities with the running components', async () => {
  stubFetch({ '/api/v1/update': {
    boot: { deploymentId: 'a'.repeat(64), kernelId: 'c'.repeat(64), rootfsId: 'd'.repeat(64), contentVerified: true, secureBoot: true, backend: 'uefi', bootVerified: true },
    state: { current: 'a'.repeat(64), candidate: 'e'.repeat(64), fallback: 'b'.repeat(64), highestGeneration: 3, failed: [] },
    lifecycle: { state: 'reboot-required' },
  } })
  renderPanel(<UpdatePanel />)
  for (const id of ['a', 'b', 'c', 'd', 'e']) expect(await screen.findByText(id.repeat(64))).toBeTruthy()
})

/// A core set is an update like a deployment, named as what it is: the one on
/// offer, the one staged, the one composed, and the one on trial with the
/// boots it has left.
it('names a core update as one and reports the sets the device holds', async () => {
  stubFetch({ '/api/v1/update': {
    boot: { deploymentId: 'a'.repeat(64), kernelId: 'c'.repeat(64), rootfsId: 'd'.repeat(64), contentVerified: true, secureBoot: true, backend: 'uefi', bootVerified: true },
    state: { current: 'a'.repeat(64), candidate: null, fallback: null, highestGeneration: 3, failed: [] },
    lifecycle: { state: 'reboot-required', available: { deploymentId: 'f'.repeat(64), version: '0.0.7', kind: 'set', setKind: 'core' }, deploymentId: 'f'.repeat(64), stagedKind: 'set', stagedSetKind: 'core' },
    sets: { core: {
      current: { id: '1'.repeat(64), channel: 'general', generation: 4, version: '0.0.6' },
      pending: { id: '2'.repeat(64), channel: 'general', generation: 5, version: '0.0.7' },
      attemptsLeft: 3,
      boot: { coreSetId: '1'.repeat(64), pending: false },
    } },
  } })
  renderPanel(<UpdatePanel />)

  expect(await screen.findByText(`core · 0.0.7 · ${'f'.repeat(64)}`)).toBeTruthy()
  expect(screen.getByText(`core · ${'f'.repeat(64)}`)).toBeTruthy()
  expect(screen.getByText('0.0.6 · general generation 4')).toBeTruthy()
  expect(screen.getByText('0.0.7 · general generation 5 · 3 boot(s) left')).toBeTruthy()
  // Installed and not booted yet, with no deployment candidate at all.
  expect(screen.getByText('An installed update awaits its first boot.')).toBeTruthy()
})

/// A set on trial with a new system says so, each retained system's own set
/// is named with it, and a newer set this root does not run is shown with why.
it('reports a paired set, the bound sets and a set waiting for a system update', async () => {
  stubFetch({ '/api/v1/update': {
    lifecycle: { state: 'reboot-required', waitsForSystem: { id: '3'.repeat(64), generation: 6, reason: 'mica-podman >= 6.1.4-7, the root provides 6.1.4-5' } },
    sets: { core: {
      current: { id: '1'.repeat(64), channel: 'general', generation: 4, version: '0.0.6' },
      pending: { id: '2'.repeat(64), channel: 'general', generation: 5, version: '0.0.7' },
      paired: 'e'.repeat(64),
      bound: { ['b'.repeat(64)]: { id: '0'.repeat(64), channel: 'general', generation: 3, version: '0.0.5' } },
      boot: { coreSetId: '1'.repeat(64), pending: false },
    } },
  } })
  renderPanel(<UpdatePanel />)

  expect(await screen.findByText('0.0.7 · general generation 5 · on trial with the new system')).toBeTruthy()
  expect(screen.getByText(`0.0.5 · general generation 3 · ${'b'.repeat(64)}`)).toBeTruthy()
  expect(screen.getByText('generation 6: mica-podman >= 6.1.4-7, the root provides 6.1.4-5')).toBeTruthy()
})

/// A core line the device could not read is said, beside the product line's
/// answer.
it('reports a core line that could not be checked', async () => {
  stubFetch({ '/api/v1/update': {
    lifecycle: { state: 'idle', coreCheckFailed: { code: 'client-failed', reason: 'set-check: Invalid component contract' } },
  } })
  renderPanel(<UpdatePanel />)

  expect(await screen.findByText('Core line could not be checked')).toBeTruthy()
  expect(screen.getByText('set-check: Invalid component contract')).toBeTruthy()
})

describe('the offline import', () => {
  /// The archive is the request body, not a form field: it is measured in
  /// hundreds of megabytes and the device writes it as it arrives.
  it('uploads the chosen archive as the body of one request', async () => {
    const fetch = stubFetch({
      '/api/v1/update': { lifecycle: { state: 'idle' } },
      'POST /api/v1/update/import': () => jsonResponse({}, 202),
    })
    renderPanel(<ManualUpdate />)

    const archive = new File([new Uint8Array([0x4d, 0x49, 0x43, 0x41])], 'release.micaupd')
    // The picker's input is hidden behind its own button, so the file is
    // handed to the element rather than to a label that points at nothing.
    const input = document.querySelector('input[type="file"]') as HTMLInputElement
    await userEvent.upload(input, archive)
    await userEvent.click(screen.getByRole('button', { name: 'Upload and import' }))

    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path]) => String(path) === '/api/v1/update/import')
      expect(found).toBeTruthy()
      return found
    })
    const init = call?.[1] as RequestInit
    expect(init.method).toBe('POST')
    // `api` builds a Headers from what it was given, so the content type is
    // read back the same way the request carries it.
    expect(new Headers(init.headers).get('content-type')).toBe('application/octet-stream')
    expect(init.body).toBe(archive)
  })
})
