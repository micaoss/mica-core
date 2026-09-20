import type { ContainerOverview, ContainerPort, ContainerUnit, ContainerVolume } from '@/lib/types'

export interface ContainerRow {
  name: string
  /// The declared entry, absent for a container the engine reports and nobody
  /// declared.
  declared?: ContainerUnit
  /// What the engine says it is doing, absent when the engine did not answer
  /// or has never heard of it.
  state?: string
}

/// Every container this device has anything to say about: the declared map and
/// the engine's list, joined by name.
///
/// Both directions matter. A declared container the engine has never heard of
/// has not started yet, and a container running that nobody declared is
/// something an operator needs to see rather than something the console should
/// quietly omit.
export function containerRows(overview: ContainerOverview | undefined): ContainerRow[] {
  const rows = new Map<string, ContainerRow>()
  for (const [name, declared] of Object.entries(overview?.declared ?? {})) {
    rows.set(name, { name, declared })
  }
  for (const entry of overview?.engine.entries ?? []) {
    // podman prints `Names` as a list; the first is the container's own name,
    // which is the `ContainerName=` the unit file declared.
    const name = entry.Names?.[0]
    if (!name) continue
    rows.set(name, { ...rows.get(name), name, state: entry.State })
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
