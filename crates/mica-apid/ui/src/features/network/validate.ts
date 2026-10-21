import { inSubnet, isIPv4, overlaps, parseCidr, poolRange, ipToInt, type IfaceEntry, type NetworkMap, type ViewKind } from './model'

/// A field the console refuses to stage as it is, with the message's key and
/// its values. The device remains the authority: these only catch what it
/// would refuse anyway, before the write.
export interface Problem { key: string; params?: Record<string, string | number> }
export type Problems = Partial<Record<'address' | 'gateway' | 'dns' | 'pool' | 'lease' | 'serverDns' | 'routes' | 'vlanId' | 'parent' | 'listenPort' | 'bitrate', Problem>>

/// The addresses other interfaces hold or will hold, for the overlap check.
export type AddressBook = { name: string; address: string }[]

export function addressBook(staged: NetworkMap, observed: Record<string, string | undefined>): AddressBook {
  const book: AddressBook = []
  for (const [name, address] of Object.entries(observed)) if (address) book.push({ name, address })
  for (const [name, entry] of Object.entries(staged)) {
    const address = entry.static?.address
    if (address && !entry.dhcp && !book.some((each) => each.name === name && each.address === address)) book.push({ name, address })
  }
  return book
}

const ipList = (items: string[] | undefined) => (items ?? []).filter((item) => !isIPv4(item))

export function validateEntry(name: string, kind: ViewKind, entry: IfaceEntry | undefined, staged: NetworkMap, book: AddressBook, port: boolean): Problems {
  const problems: Problems = {}
  if (!entry) return problems
  if (kind === 'can') {
    if (!entry.can || !(entry.can.bitrate >= 1 && entry.can.bitrate <= 1_000_000)) problems.bitrate = { key: 'network.problems.bitrate' }
    return problems
  }
  if (kind === 'vlan' && entry.vlan) {
    const { parent, id } = entry.vlan
    if (!(id >= 1 && id <= 4094)) problems.vlanId = { key: 'network.problems.vlanRange' }
    else if (Object.entries(staged).some(([other, each]) => other !== name && each.vlan?.parent === parent && each.vlan.id === id)) problems.vlanId = { key: 'network.problems.vlanTaken', params: { parent, id } }
    if (!parent) problems.parent = { key: 'network.problems.parentMissing' }
  }
  if (kind === 'wireguard') {
    const listen = entry.wireguard?.listenPort
    if (listen !== undefined && !(listen >= 1 && listen <= 65535)) problems.listenPort = { key: 'network.problems.portRange' }
    else if (listen !== undefined && Object.entries(staged).some(([other, each]) => other !== name && each.kind === 'wireguard' && each.wireguard?.listenPort === listen)) problems.listenPort = { key: 'network.problems.portTaken' }
  }
  if (port) return problems
  if (entry.dhcp) {
    if (ipList(entry.dns).length) problems.dns = { key: 'network.problems.dns', params: { list: ipList(entry.dns).join(', ') } }
  } else if (entry.static) {
    const address = entry.static.address
    if (!address) problems.address = { key: 'network.problems.addressMissing' }
    else if (!parseCidr(address)) problems.address = { key: 'network.problems.cidr' }
    else {
      const clash = book.find((each) => each.name !== name && overlaps(address, each.address))
      if (clash) problems.address = { key: 'network.problems.overlap', params: { name: clash.name, address: clash.address } }
    }
    const gateway = entry.static.gateway
    if (gateway) {
      if (!isIPv4(gateway)) problems.gateway = { key: 'network.problems.ipv4' }
      else if (!inSubnet(gateway, address)) problems.gateway = { key: 'network.problems.gatewaySubnet' }
    }
    if (ipList(entry.static.dns).length) problems.dns = { key: 'network.problems.dns', params: { list: ipList(entry.static.dns).join(', ') } }
    const server = entry.dhcpServer
    if (server && parseCidr(address)) {
      const range = poolRange(address, server)
      const own = address.split('/')[0]
      if (!range || server.poolSize < 1 || !inSubnet(range.start, address) || !inSubnet(range.end, address)) problems.pool = { key: 'network.problems.poolSubnet' }
      else if (ipToInt(own) >= ipToInt(range.start) && ipToInt(own) <= ipToInt(range.end)) problems.pool = { key: 'network.problems.poolOwn', params: { address: own } }
      if (server.leaseSeconds !== undefined && server.leaseSeconds < 60) problems.lease = { key: 'network.problems.lease' }
      if (ipList(server.dns).length) problems.serverDns = { key: 'network.problems.dns', params: { list: ipList(server.dns).join(', ') } }
    }
  }
  for (const route of entry.routes ?? []) {
    if (!parseCidr(route.destination) || (route.gateway && !isIPv4(route.gateway))) { problems.routes = { key: 'network.problems.route' }; break }
  }
  return problems
}
