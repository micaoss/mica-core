import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { SupportInfo } from '@/lib/types'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { SupportInfoPanel } from './support-info-panel'

const summary: SupportInfo = {
  schemaVersion: 1,
  generatedAt: '2026-11-12T08:30:05Z',
  device: { available: true, board: 'CX3576', kernel: '6.1.192-mica' },
  software: { available: true, product: 'cx3576.full', productVersion: '20261111-1617', osVersion: '20261111', core: '0.0.5', daemon: '0.0.5' },
  deployment: { available: false, reason: 'unavailable' },
  uptimeSeconds: 5400,
  time: { available: true, timezone: 'Europe/Athens', status: 'synchronized', synchronized: true },
  integrity: { available: true, items: [
    { key: 'trustGrade', state: 'ok', detailCode: null },
    { key: 'updateStatus', state: 'unknown', detailCode: 'check_failed' },
  ] },
  data: { available: true, state: 'ok', totalBytes: 1000, usedBytes: 250, freeBytes: 750, usedPercent: 25 },
  lastUpdateError: { available: true, code: 'network-offline', during: 'check' },
}

const expectedText = [
  'Mica support information',
  'Generated: 2026-11-12T08:30:05Z',
  'Board: CX3576',
  'Kernel: 6.1.192-mica',
  'Product: cx3576.full 20261111-1617',
  'OS version: 20261111',
  'Core: 0.0.5',
  'Daemon: 0.0.5',
  'Deployment: not available (unavailable)',
  'Uptime: 5400 s',
  'Timezone: Europe/Athens',
  'Time synchronization: synchronized',
  'Trust grade: OK',
  'Update status: Unknown (check_failed)',
  'DATA storage: OK, 250 B of 1000 B used (25%)',
  'Last update error: network-offline (check)',
].join('\n')

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

function stubClipboard() {
  const writeText = vi.fn<(text: string) => Promise<void>>(async () => undefined)
  vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } })
  return writeText
}

describe('the support information card', () => {
  it('copies the summary as labelled lines in a fixed order', async () => {
    stubFetch({ '/api/v1/system/support-info': summary })
    const writeText = stubClipboard()
    renderPanel(<SupportInfoPanel />)

    expect(screen.getByText(/carries no identifiers, addresses, credentials or logs/)).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Copy device information' }))
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1))
    expect(writeText.mock.calls[0][0]).toBe(expectedText)
    expect(await screen.findByText('Device information copied.')).toBeTruthy()
  })

  it('downloads the same summary as JSON under its generation time', async () => {
    stubFetch({ '/api/v1/system/support-info': summary })
    let saved: Blob | undefined
    vi.stubGlobal('URL', { ...URL, createObjectURL: (blob: Blob) => { saved = blob; return 'blob:support' }, revokeObjectURL: () => undefined })
    const names: string[] = []
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) { names.push(this.download) })
    renderPanel(<SupportInfoPanel />)

    await userEvent.click(screen.getByRole('button', { name: 'Download device information' }))
    await waitFor(() => expect(names).toEqual(['mica-support-20261112T083005Z.json']))
    expect(JSON.parse(await saved!.text())).toEqual(summary)
    click.mockRestore()
  })

  it('keeps a value on its own line, whatever it carries', async () => {
    stubFetch({ '/api/v1/system/support-info': { ...summary, device: { available: true, board: 'CX3576\nDeployment: confirmed', kernel: null } } })
    const writeText = stubClipboard()
    renderPanel(<SupportInfoPanel />)

    await userEvent.click(screen.getByRole('button', { name: 'Copy device information' }))
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(1))
    const lines = writeText.mock.calls[0][0].split('\n')
    expect(lines).toContain('Board: CX3576 Deployment: confirmed')
    expect(lines).toContain('Kernel: —')
    expect(lines.filter((line) => line.startsWith('Deployment:'))).toEqual(['Deployment: not available (unavailable)'])
  })

  it('copies nothing when the summary cannot be read', async () => {
    stubFetch({ '/api/v1/system/support-info': () => jsonResponse({ error: { code: 'internal', message: 'micad is not answering' } }, 503) })
    const writeText = stubClipboard()
    renderPanel(<SupportInfoPanel />)

    await userEvent.click(screen.getByRole('button', { name: 'Copy device information' }))
    expect(await screen.findByText(/micad is not answering/)).toBeTruthy()
    expect(writeText).not.toHaveBeenCalled()
  })
})
