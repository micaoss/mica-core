import { ApiError } from '@/shared/lib/http'

/// The codes a refused network write answers that have copy of their own.
const KNOWN = [
  'address_overlap', 'bridge_port_addressed', 'bridge_port_invalid', 'bridge_port_taken', 'busy', 'channel_not_allowed',
  'gateway_out_of_subnet', 'in_use', 'metric_duplicate', 'name_invalid', 'not_supported', 'version_conflict',
  'vlan_duplicate', 'vlan_parent_invalid', 'vlan_parent_unsupported',
] as const

const known = (code: string): code is (typeof KNOWN)[number] => (KNOWN as readonly string[]).includes(code)

/// What to tell the operator about a refused network write: the code's own
/// copy with the values the device named, or, for a code without one, that
/// the device refused it and the running configuration stands.
export function networkRefusal(failure: unknown, t: (key: string, params?: Record<string, unknown>) => string): string | undefined {
  if (!(failure instanceof ApiError) || !failure.code) return undefined
  const params = Object.fromEntries(Object.entries(failure.params ?? {}).map(([key, value]) => [key, Array.isArray(value) ? value.join(', ') : String(value)]))
  return known(failure.code) ? t(`network.errors.${failure.code}`, params) : t('network.errors.other', { code: failure.code })
}

/// The interfaces a refusal names, each broken rule's own.
export function refusedInterfaces(failure: unknown): string[] {
  if (!(failure instanceof ApiError)) return []
  return [...new Set((failure.details ?? []).map((detail) => detail.interface))]
}
