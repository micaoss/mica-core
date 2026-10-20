import { useQuery } from '@tanstack/react-query'
import { api } from '@/shared/lib/http'
import type { UnitEntry, UnitsOverview, UserUnit } from '@/lib/types'

export const UNITS = '/api/v1/units'

/// Where the declared services live in the settings.
export const UNITS_SETTINGS = 'services.units'

export function unitRoute(name: string) {
  return `${UNITS}/${encodeURIComponent(name)}`
}

/// What micad says of every service: declared, or put there with `mica-unit`.
export function useUnits() {
  return useQuery({ queryKey: ['units'], queryFn: () => api<UnitsOverview>(UNITS), refetchInterval: 10_000 })
}

/// What the settings declare, by name: the text a service is edited from.
export function useDeclaredUnits() {
  return useQuery({
    queryKey: ['settings', UNITS_SETTINGS],
    queryFn: () => api<Record<string, UserUnit>>(`/api/v1/settings/${UNITS_SETTINGS}`),
  })
}

/// Active is fine, failed needs a look, anything else is just a state.
export function unitTone(entry: UnitEntry | undefined) {
  if (entry?.active === 'active') return 'success'
  if (entry?.active === 'failed' || entry?.refused) return 'warning'
  return 'neutral'
}

/// A starting text for a new service of each init: one that `mica-unit`
/// accepts and that starts at boot when enabled.
export const TEMPLATES: Record<UserUnit['kind'], string> = {
  systemd: '[Unit]\nDescription=My service\n\n[Service]\nExecStart=/mica/apps/my-service/run\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n',
  openrc: '#!/sbin/openrc-run\n\ndescription="My service"\ncommand="/mica/apps/my-service/run"\ncommand_background=true\npidfile="/run/$RC_SVCNAME.pid"\n',
}
