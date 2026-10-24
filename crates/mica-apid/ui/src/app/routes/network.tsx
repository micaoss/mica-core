import { createFileRoute } from '@tanstack/react-router'
import { NetworkPage } from '@/features/network/network-page'

const TABS = ['overview', 'interfaces', 'routes', 'wifi', 'dns', 'wireguard', 'bluetooth'] as const

// The tab and the open tunnel live in the URL, so a link or a reload lands on
// the same view.
export const Route = createFileRoute('/network')({
  component: NetworkPage,
  validateSearch: (search: Record<string, unknown>): { tab?: typeof TABS[number]; tunnel?: string } => ({
    ...(TABS.includes(search.tab as typeof TABS[number]) ? { tab: search.tab as typeof TABS[number] } : {}),
    ...(typeof search.tunnel === 'string' ? { tunnel: search.tunnel } : {}),
  }),
})
