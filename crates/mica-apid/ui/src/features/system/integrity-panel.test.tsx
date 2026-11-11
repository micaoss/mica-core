import { cleanup, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { SystemIntegrity } from '@/lib/types'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { IntegrityPanel } from './integrity-panel'

const report: SystemIntegrity = {
  checkedAt: '2026-11-11T12:00:00Z',
  items: [
    { key: 'trustGrade', state: 'attention', value: 'development', detailCode: 'development', domains: ['root', 'core'] },
    { key: 'secureBoot', state: 'attention', value: false, detailCode: 'disabled' },
    { key: 'bootVerified', state: 'ok', value: true, detailCode: null },
    { key: 'contentVerified', state: 'unknown', value: null, detailCode: 'unavailable' },
    { key: 'deploymentConfirmed', state: 'attention', value: 7, detailCode: 'trial_boot' },
    { key: 'updateStatus', state: 'attention', value: '0.0.7', detailCode: 'update_available', checkedAt: '2026-11-11T06:00:00Z' },
    { key: 'dataReady', state: 'unknown', value: null, detailCode: 'source_failed' },
  ],
  extensions: [{ key: 'sshAuthorizedKeys', value: 2 }, { key: 'customUi', value: false }],
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

/// The row of the check labelled `label`.
function row(label: string) {
  return screen.getByText(label).closest('li') as HTMLElement
}

async function rowOf(label: string) {
  return (await screen.findByText(label)).closest('li') as HTMLElement
}

describe('the integrity card', () => {
  it('lists every check with its state and what it rests on', async () => {
    stubFetch({ '/api/v1/system/integrity': report })
    renderPanel(<IntegrityPanel />)

    expect(await screen.findByText('Trust grade')).toBeTruthy()
    expect(within(row('Trust grade')).getByText('Attention')).toBeTruthy()
    expect(within(row('Trust grade')).getByText('development material: root, core')).toBeTruthy()
    expect(within(row('Secure Boot')).getByText('off')).toBeTruthy()
    expect(within(row('Boot verification')).getByText('OK')).toBeTruthy()
    expect(within(row('Content verification')).getByText('Unknown')).toBeTruthy()
    expect(within(row('Content verification')).getByText('not reported')).toBeTruthy()
    expect(within(row('Deployment confirmed')).getByText('on trial, not confirmed yet · generation 7')).toBeTruthy()
    expect(within(row('Update status')).getByText('0.0.7 is available · last check 2026-11-11 06:00 UTC')).toBeTruthy()
    expect(within(row('DATA storage')).getByText('could not be read')).toBeTruthy()
    expect(screen.getByText('Read 2026-11-11 12:00 UTC.')).toBeTruthy()
    expect(screen.getByText('2 SSH authorized keys')).toBeTruthy()
    expect(screen.getByText('The built-in console')).toBeTruthy()
  })

  it('says on the confirmation check whether the last rollback took', async () => {
    const rolledBack = { ...report, items: report.items.map((item) => item.key === 'deploymentConfirmed'
      ? { ...item, rollback: { outcome: 'did_not_take', target: 'b'.repeat(64), targetGeneration: 6, targetVersion: null,
        booted: 'a'.repeat(64), bootedGeneration: 7, bootedVersion: '0.0.6', reason: null } }
      : item) }
    stubFetch({ '/api/v1/system/integrity': rolledBack })
    renderPanel(<IntegrityPanel />)

    expect(await within(await rowOf('Deployment confirmed')).findByText(/The device booted generation 7 \(0.0.6\); the rollback to generation 6 did not take\./)).toBeTruthy()
  })

  it('reads again on Refresh', async () => {
    const fetch = stubFetch({ '/api/v1/system/integrity': report })
    renderPanel(<IntegrityPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Refresh' }))
    await vi.waitFor(() => expect(fetch.mock.calls.filter(([path]) => String(path) === '/api/v1/system/integrity')).toHaveLength(2))
  })

  it('says a failed read failed, and shows no check', async () => {
    stubFetch({ '/api/v1/system/integrity': () => jsonResponse({ error: { code: 'micad_unreachable', message: 'micad is not reachable' } }, 503) })
    renderPanel(<IntegrityPanel />)

    expect(await screen.findByText(/micad is not reachable/)).toBeTruthy()
    expect(screen.queryByText('Trust grade')).toBeNull()
  })
})
