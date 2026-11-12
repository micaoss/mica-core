import { useQuery } from '@tanstack/react-query'
import { api } from '@/shared/lib/http'

/// A part of the API a product may leave out.
export type Feature = 'wifi' | 'bluetooth' | 'ssh' | 'containers' | 'mqtt'

interface ApiMeta {
  features?: Feature[]
  init?: 'systemd' | 'openrc'
  docsUrl?: string
  docsUrlEn?: string
}

/// The features this device serves, from `GET /api/v1/meta`.
///
/// A pane is hidden only once the device has said its feature is absent: until
/// the answer arrives, or if it cannot be read, everything is shown, and the
/// routes themselves answer 404 for what is not served.
export function useFeatures(): (feature: Feature) => boolean {
  const served = useMeta().data?.features
  return (feature) => served === undefined || served.includes(feature)
}

function useMeta() {
  return useQuery({
    queryKey: ['meta'],
    queryFn: () => api<ApiMeta>('/api/v1/meta'),
    staleTime: Infinity,
    retry: false,
  })
}

/// Whether the product runs OpenRC, whose network backend has no bridge,
/// VLAN, WireGuard, static routes or DHCP server. Until the device says, it
/// is taken not to.
export function useOpenrc(): boolean {
  return useMeta().data?.init === 'openrc'
}

/// The root of the product's online documentation in `language`, when the
/// device names one: its default tree in Chinese, its English tree (or the
/// default one when it has none apart) in any other language.
export function useDocsRoot(language: string): string | undefined {
  const meta = useMeta().data
  const url = language === 'zh-CN' ? meta?.docsUrl : (meta?.docsUrlEn ?? meta?.docsUrl)
  return url?.startsWith('https://') && url.endsWith('/') ? url : undefined
}
