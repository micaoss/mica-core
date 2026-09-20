import type { ObservedAddress, ObservedInterface, ObservedNetworkInterface, ObservedNetworkState } from './types'

export interface NetworkRow {
  name: string
  configured?: Record<string, unknown>
  observed?: ObservedInterface
}

export function networkRows(
  configured: Record<string, unknown> = {},
  observed: ObservedInterface[] = [],
): NetworkRow[] {
  const rows = new Map<string, NetworkRow>()
  for (const [name, value] of Object.entries(configured)) {
    rows.set(name, {
      name,
      configured: value && typeof value === 'object' ? value as Record<string, unknown> : {},
    })
  }
  for (const iface of observed) {
    const name = iface.name ?? `#${iface.index ?? 'unknown'}`
    rows.set(name, { ...rows.get(name), name, observed: iface })
  }
  return [...rows.values()].sort((left, right) => {
    const leftIndex = left.observed?.index ?? Number.MAX_SAFE_INTEGER
    const rightIndex = right.observed?.index ?? Number.MAX_SAFE_INTEGER
    return leftIndex - rightIndex || left.name.localeCompare(right.name)
  })
}

/// Interfaces the list hides unless they were declared here.
///
/// The loopback, and the virtual ends the container engine and the kernel
/// create: a device running four containers grows a `veth` per container, and
/// an operator scrolling past them to find `eth0` is the list failing at its
/// one job. A declared entry is always shown -- if someone wrote it down, it
/// belongs on the page whatever it is called.
const HIDDEN_KINDS = ['loopback', 'veth', 'dummy']
const HIDDEN_PREFIXES = ['veth', 'podman', 'cni-', 'docker', 'dummy']

/// `lo`, and the `lo<n>` spelling a kernel or a board file sometimes uses.
/// Not every name starting with `lo`: a NIC an integrator named `lolink0` is
/// theirs and belongs on the page.
function isLoopbackName(name: string) {
  return name === 'lo' || /^lo\d+$/.test(name)
}

function isHidden(row: NetworkRow) {
  const kind = row.observed?.kind ?? row.observed?.type
  if (kind && HIDDEN_KINDS.includes(kind)) return true
  return isLoopbackName(row.name) || HIDDEN_PREFIXES.some((prefix) => row.name.startsWith(prefix))
}

/// The rows an operator came to the page for: everything declared, plus the
/// physical and wireless links the device actually has.
export function visibleNetworkRows(rows: NetworkRow[]): NetworkRow[] {
  return rows.filter((row) => row.configured !== undefined || !isHidden(row))
}

/// The interfaces a VLAN can sit on and a bridge can take as a port: the links
/// the kernel already has. A VLAN, a bridge or a tunnel is not one of them --
/// networkd builds those, and naming one as a parent is how a VLAN ends up on
/// a device that does not exist yet.
export function physicalInterfaces(rows: NetworkRow[]): string[] {
  return rows
    .filter((row) => !isHidden(row))
    .filter((row) => {
      const declared = row.configured?.kind as string | undefined
      if (declared !== undefined) return declared === 'physical'
      const observed = row.observed?.kind
      // networkd reports no `Kind` for a NIC it did not create; a `Kind` of
      // `bridge`, `vlan` or `wireguard` is one it did.
      return observed === undefined
    })
    .map((row) => row.name)
}

export interface Uplink {
  name: string
  address?: string
  operationalState?: string
}

/// Whether the device's own loopback is what this entry describes. networkd
/// describes it first and it always carries an address, so anything that picks
/// "the first interface with an address" picks `lo` on every device.
function isLoopback(iface: ObservedNetworkInterface) {
  return iface.type === 'loopback' || iface.name === 'lo'
}

/// The address to show for an interface: a globally scoped one, IPv4 first
/// because that is what an operator types into a browser. A link-local or host
/// address is not an address the device is reachable at.
function globalAddress(addresses: ObservedAddress[]): string | undefined {
  const global = addresses.filter((address) => address.scope === 'global' && address.address)
  const chosen = global.find((address) => address.family === 'ipv4') ?? global[0]
  if (!chosen?.address) return undefined
  return chosen.prefixLength === undefined ? chosen.address : `${chosen.address}/${chosen.prefixLength}`
}

/// The interface the device reaches the world through: the one a default route
/// leaves by, and failing that the first globally addressed interface. The
/// loopback is never it, and neither is "no answer": an interface a default
/// route names is the uplink whether or not its address was observed.
export function selectUplink(state?: ObservedNetworkState): Uplink | undefined {
  const entries = (state?.interfaces.entries ?? []).filter((iface) => !isLoopback(iface))
  const routed = state?.defaultRoutes.entries?.find((route) => route.interface !== undefined)
  const chosen = entries.find((iface) => iface.name === routed?.interface)
    ?? entries.find((iface) => globalAddress(iface.addresses) !== undefined)
  if (!chosen) return undefined
  return {
    name: chosen.name,
    address: globalAddress(chosen.addresses),
    operationalState: chosen.link.operationalState,
  }
}

interface ConfiguredSummaryLabels {
  notConfigured: string
  physical: string
  dhcp: string
  static: string
  noAddressing: string
  format: (kind: string, method: string) => string
}

const englishSummaryLabels: ConfiguredSummaryLabels = {
  notConfigured: 'Not configured',
  physical: 'physical',
  dhcp: 'DHCP',
  static: 'Static',
  noAddressing: 'No addressing',
  format: (kind, method) => `${kind} · ${method}`,
}

export function configuredSummary(
  configured?: Record<string, unknown>,
  labels: ConfiguredSummaryLabels = englishSummaryLabels,
) {
  if (!configured) return labels.notConfigured
  const kind = typeof configured.kind === 'string' ? configured.kind : labels.physical
  const method = configured.dhcp === true
    ? labels.dhcp
    : configured.static && typeof configured.static === 'object'
      ? labels.static
      : labels.noAddressing
  return labels.format(kind, method)
}
