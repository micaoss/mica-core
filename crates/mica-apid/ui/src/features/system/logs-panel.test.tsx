import { cleanup, screen } from '@testing-library/react'
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
})
