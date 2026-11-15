/// An uplink's reachability check, as the entry stores it and as the
/// device reports it.
import type { StatusTone } from '@/shared/components/status-badge'
import type { ObservedNetworkState, UplinkObservation } from '@/lib/types'
import { isIPv4, type IfaceEntry, type ProbeConfig, type ProbeTarget } from './model'

/// What switching the check on fills in: two public DNS resolvers on
/// different networks, by TCP on port 53, which a captive portal does not
/// answer for them as it may on 443.
export const DEFAULT_TARGETS: ProbeTarget[] = [
  { address: '1.1.1.1', method: 'tcp', port: 53 },
  { address: '8.8.8.8', method: 'tcp', port: 53 },
]

export const MAX_TARGETS = 4

/// The device's defaults and bounds for the numbers.
export const PROBE_DEFAULTS = { intervalS: 10, timeoutMs: 2000, failuresToDown: 3, successesToUp: 5 }

/// The check with the defaults filled in.
export function resolved(probe: ProbeConfig | undefined) {
  return {
    enabled: probe?.enabled ?? false,
    targets: probe?.targets?.length ? probe.targets : DEFAULT_TARGETS,
    intervalS: probe?.intervalS ?? PROBE_DEFAULTS.intervalS,
    timeoutMs: probe?.timeoutMs ?? PROBE_DEFAULTS.timeoutMs,
    failuresToDown: probe?.failuresToDown ?? PROBE_DEFAULTS.failuresToDown,
    successesToUp: probe?.successesToUp ?? PROBE_DEFAULTS.successesToUp,
  }
}

/// The entry with its check switched on or off. Switched on with no
/// targets, it takes the two defaults, so the stored entry names them.
export function switchProbe(entry: IfaceEntry, on: boolean): IfaceEntry {
  const probe: ProbeConfig = { ...entry.probe, enabled: on }
  if (on && !probe.targets?.length) probe.targets = DEFAULT_TARGETS.map((target) => ({ ...target }))
  return { ...entry, probe }
}

/// About how many megabytes a day one target costs at `intervalS`: some
/// 180 bytes on the wire per probe.
export const megabytesPerDay = (intervalS: number) => Math.round((86_400 / intervalS) * 180 / 100_000) / 10

/// The field the device would refuse, as a problem key, or none.
export function probeProblem(probe: ProbeConfig | undefined): string | undefined {
  if (!probe) return undefined
  const targets = probe.targets ?? []
  const seen = new Set(targets.map((target) => `${target.address}|${target.method}|${target.port ?? ''}`))
  if ((probe.enabled && targets.length === 0) || targets.length > MAX_TARGETS || seen.size !== targets.length) return 'network.uplink.problems.targets'
  for (const target of targets) {
    if (target.address !== 'gateway' && !isIPv4(target.address)) return 'network.uplink.problems.address'
    if (target.method === 'tcp' && target.port !== undefined && !(target.port >= 1 && target.port <= 65535)) return 'network.uplink.problems.port'
  }
  const { intervalS, timeoutMs, failuresToDown, successesToUp } = resolved(probe)
  if (!(intervalS >= 2 && intervalS <= 300)) return 'network.uplink.problems.interval'
  if (!(timeoutMs >= 200 && timeoutMs <= 10_000 && timeoutMs < intervalS * 1000)) return 'network.uplink.problems.timeout'
  if (!(failuresToDown >= 1 && failuresToDown <= 20 && successesToUp >= 1 && successesToUp <= 50)) return 'network.uplink.problems.threshold'
  return undefined
}

/// What the device reports of `name`'s check, and whether it carries the
/// traffic.
export function uplinkOf(status: ObservedNetworkState | undefined, name: string): { uplink?: UplinkObservation; active: boolean; failover: boolean } {
  const uplinks = status?.uplinks
  return {
    uplink: uplinks?.uplinks.find((each) => each.name === name),
    active: uplinks?.active === name,
    failover: Boolean(uplinks?.override),
  }
}

export const STATE_TONE: Record<UplinkObservation['state'], StatusTone> = {
  up: 'success',
  down: 'danger',
  unknown: 'accent',
  off: 'neutral',
  noRoute: 'warning',
  blocked: 'warning',
}
