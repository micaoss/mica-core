import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { RollbackPanel } from './rollback-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const ACTION = 'Roll back to the previous version'
const running = 'a'.repeat(64)
const retained = 'b'.repeat(64)

/// The update state as micad answers it: deployment `a` (0.0.6, generation 2)
/// runs confirmed, `b` (0.0.5, generation 1) is retained.
function updateState(rollback: Record<string, unknown>, current: string | null = running) {
  return {
    boot: { deploymentId: running },
    state: { current, fallback: retained, candidate: null, failed: [] },
    deployments: [
      { id: running, generation: 2, version: '0.0.6', triesLeft: null },
      { id: retained, generation: 1, version: '0.0.5', triesLeft: null },
    ],
    rollback,
  }
}

const permitted = {
  target: retained, permitted: true, reason: null,
  targetVersion: '0.0.5', targetGeneration: 1, confirmed: true, requiresReboot: true,
}

async function confirmRollback() {
  await userEvent.click(await screen.findByRole('button', { name: ACTION }))
  const dialog = await screen.findByRole('alertdialog')
  await userEvent.click(within(dialog).getByRole('button', { name: ACTION }))
  return dialog
}

function rollbackCalls(fetch: ReturnType<typeof stubFetch>) {
  return fetch.mock.calls.filter(([input, init]) => String(input) === '/api/v1/update/rollback' && init?.method === 'POST')
}

describe('the guarded rollback', () => {
  it('names both versions, says what a rollback does not undo, and sends the target it showed', async () => {
    const fetch = stubFetch({
      '/api/v1/update': updateState(permitted),
      'POST /api/v1/update/rollback': () => jsonResponse({ deploymentId: running, target: retained, nextStep: 'POST /api/v1/actions/reboot' }),
    })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText('Running: 0.0.6 · generation 2 · confirmed')).toBeTruthy()
    expect(screen.getByText('Retained: 0.0.5 · generation 1 · confirmed')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: ACTION }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/from 0.0.6 to 0.0.5/)).toBeTruthy()
    expect(within(dialog).getByText(/reboot/i)).toBeTruthy()
    expect(within(dialog).getByText(/does not roll back application data or settings/)).toBeTruthy()
    await userEvent.click(within(dialog).getByRole('button', { name: ACTION }))

    await waitFor(() => expect(rollbackCalls(fetch)).toHaveLength(1))
    expect(JSON.parse(String(rollbackCalls(fetch)[0][1]?.body))).toEqual({ expectedTarget: retained })
    expect(await screen.findByText(`Deployment ${running} was rejected. The next boot uses ${retained}.`)).toBeTruthy()
    expect(screen.getByText(/does not reboot. Reboot the device to complete the rollback/)).toBeTruthy()
  })

  it('names a target without version information by its short ID, and invents no version', async () => {
    stubFetch({ '/api/v1/update': updateState({ ...permitted, targetVersion: null, targetGeneration: null, confirmed: null }) })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText(`Retained: version information unavailable (${retained.slice(0, 12)})`)).toBeTruthy()
    expect((screen.getByRole('button', { name: ACTION }) as HTMLButtonElement).disabled).toBe(false)
  })

  it('shows a refusal beside a disabled control', async () => {
    stubFetch({ '/api/v1/update': updateState({ target: null, permitted: false, reason: 'no_usable_fallback', requiresReboot: true }) })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText('rollback not available')).toBeTruthy()
    expect(screen.getByText(/no usable retained deployment/)).toBeTruthy()
    expect((screen.getByRole('button', { name: ACTION }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('does not offer a target that was never confirmed', async () => {
    stubFetch({ '/api/v1/update': updateState({ ...permitted, confirmed: false }) })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText('Retained: 0.0.5 · generation 1 · not confirmed')).toBeTruthy()
    expect(screen.getByText(/retained deployment was never confirmed/)).toBeTruthy()
    expect((screen.getByRole('button', { name: ACTION }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('says a running deployment on trial is not confirmed', async () => {
    stubFetch({ '/api/v1/update': updateState({ target: null, permitted: false, reason: 'running_not_confirmed', requiresReboot: true }, null) })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText('Running: 0.0.6 · generation 2 · not confirmed')).toBeTruthy()
  })

  it('names each refusal the deployment state can produce', async () => {
    const expected: [string, RegExp][] = [
      ['candidate_pending', /candidate deployment is pending/],
      ['running_not_confirmed', /running deployment is not confirmed/],
      ['no_usable_fallback', /no usable retained deployment/],
      ['something_new', /deployment state refuses a rollback/],
    ]
    for (const [reason, copy] of expected) {
      stubFetch({ '/api/v1/update': { rollback: { target: null, permitted: false, reason } } })
      renderPanel(<RollbackPanel />)
      expect(await screen.findByText(copy)).toBeTruthy()
      cleanup()
    }
  })

  it('reads the target again before acting, and asks again when it changed', async () => {
    let reads = 0
    const changed = 'c'.repeat(64)
    const fetch = stubFetch({
      '/api/v1/update': () => {
        reads += 1
        return jsonResponse(reads === 1 ? updateState(permitted) : updateState({ ...permitted, target: changed, targetVersion: '0.0.4' }))
      },
    })
    renderPanel(<RollbackPanel />)

    await confirmRollback()
    expect(await screen.findByText(/rollback target changed/)).toBeTruthy()
    expect(rollbackCalls(fetch)).toHaveLength(0)
    expect(await screen.findByText('Retained: 0.0.4 · generation 1 · confirmed')).toBeTruthy()
  })

  it('maps a 409 from the action onto the same refusal vocabulary, the changed target included', async () => {
    for (const [code, copy] of [['running_not_confirmed', /running deployment is not confirmed/], ['target_changed', /rollback target changed/]] as const) {
      stubFetch({
        '/api/v1/update': updateState(permitted),
        'POST /api/v1/update/rollback': () => jsonResponse({ error: { code, message: 'refused' } }, 409),
      })
      renderPanel(<RollbackPanel />)
      await confirmRollback()
      expect(await screen.findByText(copy)).toBeTruthy()
      cleanup()
    }
  })

  it('says in the boot after a rollback whether it took', async () => {
    const took = { outcome: 'rolled_back', target: retained, targetGeneration: 1, targetVersion: '0.0.5' }
    stubFetch({ '/api/v1/update': { ...updateState({ target: null, permitted: false, reason: 'no_usable_fallback' }), lastRollback: took } })
    renderPanel(<RollbackPanel />)
    expect(await screen.findByText('Rolled back to generation 1 (0.0.5).')).toBeTruthy()
    cleanup()

    const failed = { outcome: 'did_not_take', target: retained, targetGeneration: 1, targetVersion: '0.0.5',
      booted: running, bootedGeneration: 2, bootedVersion: '0.0.6', reason: 'target_failed' }
    stubFetch({ '/api/v1/update': { ...updateState(permitted), lastRollback: failed } })
    renderPanel(<RollbackPanel />)
    expect(await screen.findByText('The device booted generation 2 (0.0.6); the rollback to generation 1 (0.0.5) did not take: the target was marked failed.')).toBeTruthy()
  })

  it('surfaces a read failure as an error', async () => {
    stubFetch({ '/api/v1/update': () => jsonResponse({ error: { message: 'micad is unavailable' } }, 503) })
    renderPanel(<RollbackPanel />)

    expect(await screen.findByText('micad is unavailable')).toBeTruthy()
  })
})
