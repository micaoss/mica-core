import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { MediaImport } from './media-import'

const MEDIA = {
  generation: '1',
  items: [{
    id: 'v1', deviceId: 'd1', label: 'UPDATES', transport: 'usb', filesystem: 'vfat',
    sizeBytes: 16_106_127_360, availableBytes: 1_000, state: 'mounted_readonly', busy: false, reason: null,
  }],
}

function candidate(overrides: Record<string, unknown> = {}) {
  return { id: 'c1', mediaId: 'v1', name: 'mica-updates/release.micaupd', sizeBytes: 314_572_800, importable: true, reason: null, ...overrides }
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('import from external storage', () => {
  it('lists the archives with their volume and imports the one chosen', async () => {
    const fetch = stubFetch({
      '/api/v1/storage/media': MEDIA,
      '/api/v1/update/media': { items: [candidate()], truncated: [], unreadable: [] },
      'POST /api/v1/update/media/import': () => jsonResponse({}, 202),
    })
    renderPanel(<MediaImport />)

    expect(await screen.findByText('mica-updates/release.micaupd')).toBeTruthy()
    expect(await screen.findByText('UPDATES · 300.0 MiB')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Import' }))

    expect(await screen.findByText('The archive was copied; the device is verifying it.')).toBeTruthy()
    const call = await waitFor(() => {
      const found = fetch.mock.calls.find(([path]) => String(path) === '/api/v1/update/media/import')
      expect(found).toBeTruthy()
      return found
    })
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ mediaId: 'v1', candidateId: 'c1' })
  })

  it('cannot import an archive too large, and reports a refused import in the device\'s words', async () => {
    stubFetch({
      '/api/v1/storage/media': MEDIA,
      '/api/v1/update/media': {
        items: [candidate(), candidate({ id: 'c2', name: 'huge.micaupd', importable: false, reason: 'archive_too_large' })],
        truncated: [], unreadable: [],
      },
      'POST /api/v1/update/media/import': () => jsonResponse({ error: { code: 'media_candidate_changed', message: 'the archive is not on the volume as it was listed' } }, 409),
    })
    renderPanel(<MediaImport />)

    expect(await screen.findByText(/larger than this device accepts/)).toBeTruthy()
    const buttons = screen.getAllByRole('button', { name: 'Import' }) as HTMLButtonElement[]
    expect(buttons.map((button) => button.disabled)).toEqual([false, true])
    await userEvent.click(buttons[0])
    expect(await screen.findByText('the archive is not on the volume as it was listed')).toBeTruthy()
  })

  it('says when no archive is plugged in, and when a volume was listed in part', async () => {
    stubFetch({
      '/api/v1/storage/media': MEDIA,
      '/api/v1/update/media': { items: [], truncated: ['v1'], unreadable: [] },
    })
    renderPanel(<MediaImport />)

    expect(await screen.findByText('No update archive is on the USB sticks or SD cards plugged in.')).toBeTruthy()
    expect(screen.getByText('Only the first 100 archives of a volume are listed.')).toBeTruthy()
  })
})
