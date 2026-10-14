import type { ContainerOverview, ContainerPort, ContainerUnit, ContainerVolume } from '@/lib/types'

export interface ContainerRow {
  name: string
  /// The declared entry, absent for a container mica-containerd still holds
  /// and the settings no longer declare: one it is removing.
  declared?: ContainerUnit
  /// mica-containerd's phase, absent when it did not answer or has not taken
  /// the declaration yet.
  state?: string
  /// The container's own health check, when it has one.
  health?: string
  /// How often the daemon restarted it.
  restarts?: number
  /// Its last exit code, meaningful once it has stopped on its own.
  exitCode?: number
}

/// Every container this device has anything to say about: the declared map and
/// mica-containerd's list, joined by name.
///
/// Both directions matter. A declared container the daemon has not taken yet
/// has not started, and one the daemon still holds that the settings no longer
/// declare is being removed; an operator needs to see either rather than have
/// the console quietly omit it.
export function containerRows(overview: ContainerOverview | undefined): ContainerRow[] {
  const rows = new Map<string, ContainerRow>()
  for (const [name, declared] of Object.entries(overview?.declared ?? {})) {
    rows.set(name, { name, declared })
  }
  for (const entry of overview?.engine.entries ?? []) {
    const name = entry.spec?.name
    if (!name) continue
    rows.set(name, {
      ...rows.get(name),
      name,
      state: entry.phase,
      health: entry.health || undefined,
      restarts: entry.restarts,
      exitCode: entry.exit_code,
    })
  }
  return [...rows.values()].sort((left, right) => left.name.localeCompare(right.name))
}

/// A comma or whitespace separated list, as the form takes it.
export function parseList(value: string): string[] {
  return value.split(/[,\s]+/).map((item) => item.trim()).filter(Boolean)
}

/// `KEY=VALUE` pairs, comma separated. A pair with no `=` is dropped rather
/// than stored as an empty value: the device would accept it and the operator
/// meant something else.
export function parseEnvironment(value: string): Record<string, string> {
  const entries: Record<string, string> = {}
  for (const item of value.split(',')) {
    const [key, ...rest] = item.split('=')
    if (rest.length === 0 || key.trim() === '') continue
    entries[key.trim()] = rest.join('=').trim()
  }
  return entries
}

export function formatEnvironment(entries: Record<string, string>): string {
  return Object.entries(entries).map(([key, value]) => `${key}=${value}`).join(', ')
}

/// `host:container[/proto]`, comma separated.
export function parsePorts(value: string): ContainerPort[] {
  const ports: ContainerPort[] = []
  for (const item of value.split(',')) {
    const text = item.trim()
    if (!text) continue
    const [pair, proto] = text.split('/')
    const [host, container] = pair.split(':')
    const hostPort = Number(host)
    const containerPort = Number(container ?? host)
    if (!Number.isInteger(hostPort) || !Number.isInteger(containerPort)) continue
    ports.push({ host: hostPort, container: containerPort, protocol: proto === 'udp' ? 'udp' : 'tcp' })
  }
  return ports
}

export function formatPorts(ports: ContainerPort[]): string {
  return ports.map((port) => `${port.host}:${port.container}${port.protocol === 'udp' ? '/udp' : ''}`).join(', ')
}

/// `hostPath:containerPath[:ro]`, comma separated. The host path bound to
/// `/mica/` is the device's rule, enforced there; this only parses.
export function parseVolumes(value: string): ContainerVolume[] {
  const volumes: ContainerVolume[] = []
  for (const item of value.split(',')) {
    const text = item.trim()
    if (!text) continue
    const [host, container, mode] = text.split(':')
    if (!host || !container) continue
    volumes.push({ host, container, readOnly: mode === 'ro' })
  }
  return volumes
}

export function formatVolumes(volumes: ContainerVolume[]): string {
  return volumes.map((volume) => `${volume.host}:${volume.container}${volume.readOnly ? ':ro' : ''}`).join(', ')
}
