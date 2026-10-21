import { useQuery } from '@tanstack/react-query'
import { api } from '@/shared/lib/http'
import type { AppEntry, AppsOverview } from '@/lib/types'

export const APPS = '/api/v1/apps'

export function appRoute(name: string) {
  return `${APPS}/${encodeURIComponent(name)}`
}

/// Every app micad keeps, re-read while a task may be changing them.
export function useApps() {
  return useQuery({ queryKey: ['apps'], queryFn: () => api<AppsOverview>(APPS), refetchInterval: 10_000 })
}

/// Every unit active is fine, one failed needs a look, anything else is a state.
export function appTone(app: AppEntry | undefined) {
  const states = app?.units.map((unit) => unit.active) ?? []
  if (app?.last?.outcome === 'failed' || states.includes('failed')) return 'warning'
  if (states.length > 0 && states.every((state) => state === 'active')) return 'success'
  return 'neutral'
}

/// One word for what an app's units are doing.
export function appState(app: AppEntry) {
  const states = app.units.map((unit) => unit.active ?? 'unknown')
  if (states.length === 0) return 'files'
  if (states.includes('failed')) return 'failed'
  return states.every((state) => state === states[0]) ? states[0] : 'mixed'
}
