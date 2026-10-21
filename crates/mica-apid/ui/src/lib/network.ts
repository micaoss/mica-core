import type { ObservedAddress, ObservedNetworkInterface, ObservedNetworkState } from './types'

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

export function isHiddenInterface(name: string, kind: string | undefined) {
  if (kind && HIDDEN_KINDS.includes(kind)) return true
  return isLoopbackName(name) || HIDDEN_PREFIXES.some((prefix) => name.startsWith(prefix))
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
