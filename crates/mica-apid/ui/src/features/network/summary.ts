import { defaultEntry, type IfaceEntry, type ViewKind } from './model'

export type Translate = (key: string, params?: Record<string, unknown>) => string

/// One line of a configuration as the review lists it: label, value.
export type SummaryRow = [label: string, value: string]
/// One changed line: label, before, after, and for a list on both sides the
/// entries added and removed.
export type DiffRow = [label: string, before: string, after: string, items?: { added: string[]; removed: string[] }]

/// An interface's configuration in the words its page uses. An interface
/// nobody declared reads as the image's default; fields the page does not
/// show are not listed.
export function summary(entry: IfaceEntry | undefined, kind: ViewKind, t: Translate): SummaryRow[] {
  const field = (name: string) => t(`network.fields.${name}`)
  const value = (name: string) => t(`network.values.${name}`)
  const list = (items: string[] | undefined) => items?.length ? items.join(', ') : value('empty')
  const rows: SummaryRow[] = [[field('managed'), value(entry === undefined ? 'imageDefault' : 'managed')]]
  const shown = entry ?? defaultEntry(kind)
  if (kind === 'wireguard') {
    rows.push([field('tunnelAddress'), shown.static?.address || value('empty')])
    rows.push([field('listenPort'), shown.wireguard?.listenPort?.toString() ?? value('automatic')])
    return rows
  }
  if (kind === 'can') {
    const can = shown.can
    rows.push([field('bitrate'), can ? `${can.bitrate}` : value('notSet')])
    rows.push([field('restart'), can?.restartMs ? `${can.restartMs} ms` : value('never')])
    rows.push([field('fd'), value(can?.fd ? 'on' : 'off')])
    if (can?.fd) rows.push([field('dataBitrate'), can.dataBitrate?.toString() ?? value('notSet')])
    return rows
  }
  if (kind === 'vlan') rows.push([field('parent'), shown.vlan?.parent ?? value('empty')], [field('vlanId'), String(shown.vlan?.id ?? '')])
  if (kind === 'bridge') rows.push([field('ports'), shown.bridge?.ports.length ? shown.bridge.ports.join(', ') : value('none')])
  if (shown.dhcp) {
    rows.push([field('method'), value('dhcp')])
    rows.push([field('customDns'), value(shown.dns?.length ? 'on' : 'off')])
    if (shown.dns?.length) rows.push([field('dns'), list(shown.dns)])
  } else if (shown.static) {
    rows.push([field('method'), value('static')])
    rows.push([field('address'), shown.static.address || value('empty')])
    rows.push([field('gateway'), shown.static.gateway || value('none')])
    rows.push([field('dns'), list(shown.static.dns)])
  } else {
    rows.push([field('method'), value('noAddress')])
  }
  rows.push([field('routes'), shown.routes?.length ? routeItems(shown, t).join('; ') : value('none')])
  return rows
}

function routeItems(entry: IfaceEntry, t: Translate): string[] {
  return (entry.routes ?? []).map((route) => `${route.destination} → ${route.gateway || t('network.values.onLink')}`)
}

/// The list fields of a configuration, by label, as their entries.
function lists(entry: IfaceEntry | undefined, t: Translate): Map<string, string[]> {
  const found = new Map<string, string[]>()
  if (!entry) return found
  const dns = entry.dhcp ? entry.dns : entry.static?.dns
  if (dns?.length) found.set(t('network.fields.dns'), dns)
  if (entry.routes?.length) found.set(t('network.fields.routes'), routeItems(entry, t))
  return found
}

/// The lines that differ between two configurations of one interface. A line
/// that only stops applying, as the DHCP options do after a switch to static,
/// is not listed unless nothing else changed.
export function diffRows(before: IfaceEntry | undefined, after: IfaceEntry | undefined, kind: ViewKind, t: Translate): DiffRow[] {
  const left = new Map(summary(before, kind, t))
  const right = new Map(summary(after, kind, t))
  const labels = [...new Set([...left.keys(), ...right.keys()])]
  const leftLists = lists(before, t)
  const rightLists = lists(after, t)
  const rows: DiffRow[] = labels
    .filter((label) => left.get(label) !== right.get(label))
    .map((label) => {
      const was = leftLists.get(label)
      const now = rightLists.get(label)
      const items = was && now ? { added: now.filter((item) => !was.includes(item)), removed: was.filter((item) => !now.includes(item)) } : undefined
      return items ? [label, left.get(label) ?? '—', right.get(label) ?? '—', items] : [label, left.get(label) ?? '—', right.get(label) ?? '—']
    })
  const shown = rows.filter(([, , value]) => value !== '—')
  return shown.length ? shown : rows
}

/// The lines a new interface is created with: its identity, and whatever
/// differs from a fresh interface of its kind.
export function createdRows(entry: IfaceEntry, kind: ViewKind, t: Translate): SummaryRow[] {
  const fresh = new Map(summary(defaultEntry(kind), kind, t))
  const always = ['parent', 'vlanId', 'ports', 'method', 'tunnelAddress', 'listenPort'].map((name) => t(`network.fields.${name}`))
  return summary(entry, kind, t).slice(1).filter(([label, value]) => always.includes(label) || fresh.get(label) !== value)
}
