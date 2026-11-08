import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { CloudAdminsSection } from './cloud-admins-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the cloud administrators switch', () => {
  it('is not shown on a product that grants nothing', async () => {
    const fetch = stubFetch({ '/api/v1/state/cloudAdmins': { enabled: false, source: 'product', productDefault: false } })
    renderPanel(<CloudAdminsSection />)
    await waitFor(() => expect(fetch).toHaveBeenCalled())
    expect(screen.queryByRole('switch')).toBeNull()
  })

  it('shows the product grant and takes it away', async () => {
    const fetch = stubFetch({
      '/api/v1/state/cloudAdmins': { enabled: true, source: 'product', productDefault: true },
      'PUT /api/v1/settings/access.cloudAdmins.enabled': () => jsonResponse({ taskId: 'task-1' }, 202),
    })
    renderPanel(<CloudAdminsSection />)
    const toggle = await screen.findByRole('switch', { name: 'Cloud SSH users are administrators' })
    expect(toggle.getAttribute('aria-checked')).toBe('true')
    expect(screen.getByText('Product default')).toBeTruthy()
    await userEvent.click(toggle)
    expect((await screen.findAllByText('Cloud SSH users are no longer administrators.')).length).toBeGreaterThan(0)
    const put = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')
    expect((put?.[1] as RequestInit).body).toBe('false')
  })

  it('stays shown once set, on any product', async () => {
    stubFetch({ '/api/v1/state/cloudAdmins': { enabled: false, source: 'setting', productDefault: false } })
    renderPanel(<CloudAdminsSection />)
    expect(await screen.findByText('Set here')).toBeTruthy()
  })
})
