import { cleanup, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderPanel, stubFetch } from '@/shared/testing/panel'
import { LogsPanel } from './logs-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the logs panel', () => {
  it('shows the lines the device served for the chosen log', async () => {
    stubFetch({
      '/api/v1/meta': { features: ['ssh'] },
      '/api/v1/system/logs/micad': { source: 'micad', available: true, lines: ['10:00 micad[1]: serving', '10:01 micad[1]: applied'], truncated: false },
    })
    renderPanel(<LogsPanel />)
    const lines = await screen.findByLabelText('Log lines')
    expect(lines.textContent).toContain('micad[1]: serving')
    expect(lines.textContent).toContain('micad[1]: applied')
  })

  it('says why a log could not be read instead of showing it empty', async () => {
    stubFetch({
      '/api/v1/meta': { features: [] },
      '/api/v1/system/logs/micad': { source: 'micad', available: false, detail: 'the log did not answer within 5s' },
    })
    renderPanel(<LogsPanel />)
    expect(await screen.findByText(/the log did not answer within 5s/)).toBeTruthy()
    expect(screen.queryByLabelText('Log lines')).toBeNull()
  })

  it('reads the previous boot when asked, and says when none is kept', async () => {
    stubFetch({
      '/api/v1/meta': { features: [] },
      '/api/v1/system/logs/micad': { source: 'micad', available: true, lines: ['10:00 micad[1]: serving'], truncated: false },
      '/api/v1/system/logs/micad?boot=previous': { source: 'micad', available: true, lines: ['09:59 micad[1]: stopping'], truncated: false },
      '/api/v1/system/logs/apid?boot=previous': { source: 'apid', available: false, detail: 'no previous boot is kept (persistent logs are off, or none was kept yet)' },
    })
    renderPanel(<LogsPanel />)
    expect((await screen.findByLabelText('Log lines')).textContent).toContain('serving')

    await userEvent.click(screen.getByRole('combobox', { name: 'Boot' }))
    await userEvent.click(await screen.findByRole('option', { name: 'Previous boot' }))
    await screen.findByText(/micad\[1\]: stopping/)

    await userEvent.click(screen.getByRole('combobox', { name: 'Log' }))
    await userEvent.click(await screen.findByRole('option', { name: 'API daemon' }))
    expect(await screen.findByText(/no previous boot is kept/)).toBeTruthy()
    expect(screen.queryByLabelText('Log lines')).toBeNull()
  })
})
