import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Plus } from 'lucide-react'
import { cn } from '@/shared/lib/utils'
import { Button } from '@/shared/components/ui/button'
import { CreateWizard } from './create-wizard'
import { bridgeOf, vlansOn } from './model'
import { Led, ROLES, roleOf, Tag, TEXT, useKindLabel, useStatusText, type Role } from './parts'
import type { NetworkData } from './use-network'
import { computeStatus, exitOf } from './view'

/// Every interface, grouped by what it is: Ethernet ports, virtual ports
/// (bridges and VLANs), radios, tunnels and field buses.
export function InterfacesTab({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const [adding, setAdding] = useState(false)
  const view = data.staged
  const groups = ROLES
    .map((role) => [role, view.order.filter((name) => roleOf(view.ifaces[name]) === role)] as [Role, string[]])
    .filter(([, names]) => names.length > 0)
  return (
    <>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="text-sm text-muted-foreground">{t('network.list.count', { count: view.order.length })}</span>
        {data.openrc ? null : <Button size="sm" onClick={() => setAdding(true)}><Plus />{t('network.list.add')}</Button>}
      </div>
      {groups.map(([role, names]) => (
        <section key={role} className="grid gap-3" aria-label={t(`network.roles.${role}`)}>
          <h2 className="flex items-baseline gap-2 text-base font-semibold">{t(`network.roles.${role}`)}<span className="text-sm font-normal text-muted-foreground">{names.length}</span></h2>
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {(role === 'phys' ? [...names].sort((left, right) => Number(right === exitOf(data.applied)) - Number(left === exitOf(data.applied))) : names)
              .map((name) => <InterfaceCard key={name} data={data} name={name} />)}
          </div>
        </section>
      ))}
      {adding ? <CreateWizard data={data} kinds={['bridge', 'vlan']} onClose={() => setAdding(false)} /> : null}
    </>
  )
}

function InterfaceCard({ data, name }: { data: NetworkData; name: string }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const kindLabel = useKindLabel()
  const text = useStatusText()
  const iface = data.staged.ifaces[name]
  const applied = data.applied.ifaces[name]
  const status = computeStatus(data.applied, name)
  const pending = data.pending.find((change) => change.name === name)
  const map = data.staged.map
  const bridge = bridgeOf(map, name)
  const vlans = vlansOn(map, name)
  const entry = iface.entry
  const obs = applied?.obs
  // Radios and tunnels are configured on their own tabs.
  const open = () => iface.kind === 'wifi'
    ? navigate({ to: '/network', search: { tab: 'wifi' } })
    : iface.kind === 'wireguard' && applied
      ? navigate({ to: '/network', search: { tab: 'wireguard', tunnel: name } })
      : navigate({ to: '/network/$name', params: { name } })
  const wanted = !entry.dhcp && entry.static?.address
  const address = bridge
    ? <span className="text-muted-foreground">{t('network.list.byBridge', { bridge })}</span>
    : obs?.address
      ? <span><span className="font-mono">{obs.address}</span> <span className="text-muted-foreground">{obs.source}</span></span>
      : wanted
        ? <span className="text-muted-foreground"><span className="font-mono">{wanted}</span> · {t('network.list.notInEffect')}</span>
        : <span className="text-muted-foreground">{t(entry.dhcp ? 'network.list.waitingDhcp' : 'network.list.noAddress')}</span>
  const note = text.note(status)
  const facts: [string, React.ReactNode][] = iface.kind === 'physical'
    ? [
        [t('network.fields.link'), <span className={obs?.carrier ? 'text-success' : 'text-destructive'}>{t(!obs ? 'network.list.missing' : obs.carrier ? 'network.list.linkUp' : 'network.list.noCable')}</span>],
        [t('network.fields.mac'), <span className="font-mono">{obs?.mac ?? '—'}</span>],
        [t('network.fields.address'), address],
        [t('network.fields.method'), bridge ? '—' : t(entry.dhcp ? 'network.values.dhcp' : 'network.values.static')],
      ]
    : iface.kind === 'can'
      ? [
          [t('network.fields.state'), <span className={TEXT[status.tone]}>{text.label(status)}</span>],
          [t('network.fields.bitrate'), entry.can ? `${entry.can.bitrate}` : t('network.values.notSet')],
        ]
      : []
  return (
    <div
      role="link"
      tabIndex={0}
      aria-label={name}
      onClick={() => void open()}
      onKeyDown={(event) => { if (event.key === 'Enter') void open() }}
      className={cn('grid cursor-pointer gap-2 rounded-xl border bg-card p-4 text-left transition-colors hover:border-primary/50 focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none', pending && 'border-warning/60')}
    >
      <div className="flex flex-wrap items-center gap-2">
        <Led tone={status.tone} />
        <span className="font-mono font-semibold">{name}</span>
        <span className="text-xs text-muted-foreground">{kindLabel(iface)}</span>
        <span className="flex-1" />
        {exitOf(data.applied) === name ? <Tag tone="exit">{t('network.tags.exit')}</Tag> : null}
        {data.session === name ? <Tag tone="info">{t('network.tags.session')}</Tag> : null}
        {!iface.declared && iface.kind !== 'wifi' ? <Tag>{t('network.tags.imageDefault')}</Tag> : null}
        {pending ? <Tag tone="pending">{t(pending.op === 'create' ? 'network.tags.pendingCreate' : 'network.tags.pending')}</Tag> : null}
      </div>
      {facts.length > 0 ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          {facts.map(([label, value]) => <div key={label} className="contents"><dt className="text-muted-foreground">{label}</dt><dd className="min-w-0 break-words">{value}</dd></div>)}
        </dl>
      ) : (
        <>
          <div className="text-sm">{iface.kind === 'wifi' ? obs?.address ? <span className="font-mono">{obs.address}</span> : <span className="text-muted-foreground">{t('network.list.noAddress')}</span> : address}</div>
          <div className="text-sm">{purpose(data, name, t)}</div>
          <div className={cn('text-sm', TEXT[status.tone])}>{text.label(status)}{note ? <span className="text-muted-foreground"> · {note}</span> : null}</div>
        </>
      )}
      {iface.kind === 'bridge' || iface.kind === 'vlan' || vlans.length > 0 || bridge ? (
        <div className="flex flex-wrap gap-1.5 border-t pt-2">
          {iface.kind === 'bridge' ? (entry.bridge?.ports.length ? entry.bridge.ports.map((port) => <Member key={port} data={data} name={port} label={t('network.list.port')} />) : <span className="text-sm text-muted-foreground">{t('network.list.noPorts')}</span>) : null}
          {iface.kind === 'vlan' && entry.vlan ? <Member data={data} name={entry.vlan.parent} label={t('network.list.on')} /> : null}
          {bridge ? <Member data={data} name={bridge} label={t('network.list.memberOf')} /> : null}
          {vlans.map((vlan) => <Member key={vlan} data={data} name={vlan} label={t('network.kinds.vlanId', { id: map[vlan].vlan?.id ?? '' })} />)}
        </div>
      ) : null}
    </div>
  )
}

function purpose(data: NetworkData, name: string, t: (key: string, params?: Record<string, unknown>) => string) {
  const iface = data.staged.ifaces[name]
  if (iface.kind === 'wifi') return data.wifi.mode === 'ap' ? t('network.list.wifiAp', { ssid: data.wifi.apSsid ?? '' }) : data.wifi.mode === 'client' ? t('network.list.wifiClient') : t('network.status.off')
  if (iface.kind === 'wireguard') return t('network.list.tunnel', { count: iface.entry.wireguard?.peers.length ?? 0 })
  if (!iface.entry.dhcp && !iface.entry.static) return t('network.values.noAddress')
  return t(iface.entry.dhcp ? 'network.values.dhcp' : 'network.values.static')
}

/// A related interface, as a chip that opens it.
function Member({ data, name, label }: { data: NetworkData; name: string; label: string }) {
  const navigate = useNavigate()
  const text = useStatusText()
  const status = computeStatus(data.applied, name)
  return (
    <button
      type="button"
      className="inline-flex items-center gap-1.5 rounded-md border px-2 py-0.5 text-xs hover:border-primary/50"
      onClick={(event) => { event.stopPropagation(); void navigate({ to: '/network/$name', params: { name } }) }}
      title={text.note(status) ?? text.label(status)}
    >
      <Led tone={status.tone} /><span className="font-mono">{name}</span><span className="text-muted-foreground">{label}</span>
      {status.tone !== 'success' ? <span className={TEXT[status.tone]}>{text.label(status)}</span> : null}
    </button>
  )
}
