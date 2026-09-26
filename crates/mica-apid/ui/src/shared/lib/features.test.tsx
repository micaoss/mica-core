import { cleanup, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, stubFetch } from '@/shared/testing/panel'
import { useFeatures } from './features'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>
}

describe('the features a device serves', () => {
  it('hides what meta leaves out, once meta has answered', async () => {
    stubFetch({ '/api/v1/meta': { api: 'v1', features: ['mqtt', 'containers'] } })
    const { result } = renderHook(() => useFeatures(), { wrapper })
    // Nothing is hidden before the device has said anything.
    expect(result.current('wifi')).toBe(true)
    await waitFor(() => expect(result.current('wifi')).toBe(false))
    expect(result.current('bluetooth')).toBe(false)
    expect(result.current('ssh')).toBe(false)
    expect(result.current('mqtt')).toBe(true)
    expect(result.current('containers')).toBe(true)
  })

  it('shows everything when meta cannot be read', async () => {
    const fetch = stubFetch({ '/api/v1/meta': () => jsonResponse({ error: { code: 'unavailable' } }, 503) })
    const { result } = renderHook(() => useFeatures(), { wrapper })
    await waitFor(() => expect(fetch).toHaveBeenCalled())
    expect(result.current('wifi')).toBe(true)
    expect(result.current('ssh')).toBe(true)
  })
})
