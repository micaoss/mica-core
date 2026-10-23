import { useState, type ReactNode } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ArrowLeftRight, ChevronDown, ChevronRight, Globe } from 'lucide-react'
import { cn } from '@/shared/lib/utils'
import type { StatusTone } from '@/shared/components/status-badge'
import { DataTable } from '@/shared/components/data-table'
import { Badge } from '@/shared/components/ui/badge'
import { Button } from '@/shared/components/ui/button'
import { dnsLinks, ProbeText, useDns } from './dns-tab'
import { bridgeOf, vlansOn } from './model'
import { Led, TEXT, useKindLabel, useStatusText } from './parts'
import type { NetworkData } from './use-network'
import { computeStatus, exitOf, routeTable, type IfaceStatus, type RouteRow } from './view'
import type { NetworkTab } from './network-page'

/// What the device runs now: the way out to the internet, the ports, DNS, the
/// radio and the tunnels. Built on the stored map and the observation, never
/// on staged changes.
export function OverviewTab({ data, onTab }: { data: NetworkData; onTab: (tab: NetworkTab) => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const kindLabel = useKindLabel()
  const text = useStatusText()
  const [showRoutes, setShowRoutes] = useState(true)
  const [allRoutes, setAllRoutes] = useState(false)
  const view = data.applied
  const open = (name: string) => void navigate({ to: '/network/$name', params: { name } })
  const status = (name: string) => computeStatus(view, name)
  const exit = view.routes[0]
  const physical = view.order.filter((name) => view.ifaces[name].kind === 'physical')
  const uplink = exitOf(view) ?? physical.find((name) => !bridgeOf(view.map, name) && view.ifaces[name].entry.dhcp)
  const standby = [...new Set(view.routes.map((route) => route.interface))].filter((name) => name !== uplink && view.ifaces[name])
  const routes = routeTable(view)
  const shownRoutes = allRoutes ? routes : routes.slice(0, ROUTES_FOLDED)
  const lan = view.order.filter((name) => view.ifaces[name].kind === 'bridge' || view.ifaces[name].kind === 'vlan')
  const tunnels = view.order.filter((name) => view.ifaces[name].kind === 'wireguard')
  const radio = view.wifi.radio && view.ifaces[view.wifi.radio] ? view.wifi.radio : undefined
  const toneOf = (names: string[]): StatusTone => !names.length ? 'neutral' : names.some((name) => status(name).tone === 'danger') ? 'danger' : names.some((name) => status(name).tone === 'warning') ? 'warning' : 'success'
  const up = uplink ? status(uplink) : undefined
  const wifi = view.wifi
  const wifiSummary = wifi.mode === 'off' ? t('network.overview.wifiOff') : wifi.mode === 'ap' ? t('network.overview.wifiAp', { count: wifi.stations }) : wifi.association ? t('network.overview.wifiJoined', { ssid: wifi.association.ssid ?? '' }) : t('network.overview.wifiClientDown')
  // A client that holds a default route is an uplink, not a downstream link.
  const wifiDownstream = !(wifi.mode === 'client' && radio && (radio === uplink || standby.includes(radio)))
  const dns = useDns()
  const globalDns = dns.data?.mode === 'global'
  const dnsGroups = dnsLinks(data, dns.data).filter((link) => link.servers.length > 0)

  return (
    <div className="grid gap-4">
      <div className="grid items-center gap-3 rounded-xl border bg-card p-4 lg:grid-cols-[1fr_auto_1fr_auto_auto_auto_auto_1.3fr]">
        <Node tone={exit ? 'success' : 'danger'} icon={<Globe />} title={t('network.overview.internet')} value={t(exit ? 'network.overview.online' : 'network.overview.offline')} />
        <Wire tone={exit ? 'success' : 'danger'} label={exit ? t('network.overview.via', { gateway: exit.gateway ?? '—' }) : t('network.overview.noDefault')} />
        <Node
          tone={up?.tone ?? 'danger'}
          icon={<ArrowLeftRight />}
          title={t('network.overview.uplink', { name: uplink ?? '—' })}
          value={uplink && view.ifaces[uplink].obs?.address ? <span className="font-mono">{view.ifaces[uplink].obs?.address}</span> : up ? text.label(up) : t('network.overview.noUplink')}
          onClick={uplink ? () => open(uplink) : undefined}
        />
        {standby.length ? (
          <div className="grid gap-2">
            {standby.map((name) => (
              <Node
                key={name}
                faint
                tone={status(name).tone}
                icon={<ArrowLeftRight />}
                title={t('network.overview.standby', { name })}
                value={view.ifaces[name].obs?.address ? <span className="font-mono">{view.ifaces[name].obs?.address}</span> : text.label(status(name))}
                onClick={() => open(name)}
              />
            ))}
          </div>
        ) : null}
        <Wire tone={up?.tone ?? 'danger'} />
        <div className="grid justify-items-center gap-0.5 rounded-lg border bg-muted/40 px-5 py-3 text-center">
          <strong className="text-sm">mica</strong>
          <span className="font-mono text-xs text-muted-foreground">{data.hostname ?? '—'}</span>
        </div>
        <Wire tone={toneOf([...lan, ...(radio && wifiDownstream ? [radio] : []), ...tunnels])} />
        <div className="grid gap-1.5 text-sm">
          {([
            [t('network.overview.virtual'), t('network.overview.healthy', { up: lan.filter((name) => status(name).tone === 'success').length, count: lan.length }), toneOf(lan)],
            ...(wifiDownstream ? [['Wi-Fi', wifiSummary, wifi.mode === 'off' || !radio ? 'neutral' : status(radio).tone]] : []),
            [t('network.overview.tunnels'), t('network.overview.tunnelCount', { count: tunnels.length }), toneOf(tunnels)],
          ] as [string, string, StatusTone][]).map(([label, value, tone]) => (
            <div key={label} className="flex items-center gap-2"><Led tone={tone} /><span className="text-muted-foreground">{label}</span><strong className="ms-auto font-medium">{value}</strong></div>
          ))}
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-[2fr_1fr]">
        <Card title={t('network.roles.phys')} aside={t('network.overview.portsLinked', { count: physical.length, up: physical.filter((name) => view.ifaces[name].obs?.carrier).length })}>
          {physical.length ? (
            <div className="grid gap-3 sm:grid-cols-2">
              {physical.map((name) => {
                const iface = view.ifaces[name]
                const bridge = bridgeOf(view.map, name)
                const vlans = vlansOn(view.map, name)
                const uses = [
                  ...(name === uplink ? [t(exit?.interface === name ? 'network.overview.useExit' : 'network.overview.useUplink')] : []),
                  ...(bridge ? [t('network.overview.useBridge', { bridge })] : []),
                  ...(vlans.length ? [t('network.status.notes.carriesVlan', { vlans: vlans.join(', ') })] : []),
                ]
                const address = !iface.obs ? t('network.list.missing') : bridge ? t('network.list.byBridge', { bridge }) : iface.obs.address ? `${iface.obs.address} ${iface.obs.source ?? ''}`.trim() : '—'
                return (
                  <dl key={name} className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-lg border p-3 text-sm">
                    <dt className="text-muted-foreground">{t('network.overview.interface')}</dt><dd><button type="button" className="font-mono font-medium text-primary hover:underline" onClick={() => open(name)}>{name}</button></dd>
                    <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className={TEXT[status(name).tone]}>{text.label(status(name))}</dd>
                    <dt className="text-muted-foreground">{t('network.overview.use')}</dt><dd>{uses.length ? uses.join(' · ') : t('network.overview.useStandalone')}</dd>
                    <dt className="text-muted-foreground">{t('network.fields.address')}</dt><dd className="font-mono break-all">{address}</dd>
                  </dl>
                )
              })}
            </div>
          ) : <Empty>{t('network.overview.noPorts')}</Empty>}
        </Card>
        <Card title={t('network.overview.virtual')} aside={t('network.list.count', { count: lan.length })}>
          {lan.length ? lan.map((name) => <Row key={name} name={name} kind={kindLabel(view.ifaces[name])} status={status(name)} onClick={() => open(name)} />) : <Empty>{t('network.overview.noVirtual')}</Empty>}
        </Card>
      </div>

      <div className="grid gap-4 lg:grid-cols-3">
        <Card title="DNS" action={<Button size="xs" variant="link" onClick={() => onTab('dns')}>{t('network.overview.dnsLink')}</Button>}>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">{t('network.dns.mode')}</dt><dd>{t(globalDns ? 'network.dns.globalMode' : 'network.dns.perInterface')}</dd>
            <dt className="text-muted-foreground">{t('network.dns.servers')}</dt>
            <dd className="grid gap-0.5">{globalDns
              ? (dns.data?.effective.global ?? []).map((server) => <span key={server} className="font-mono text-[0.8125rem]">{server}</span>)
              : dnsGroups.length ? dnsGroups.map((group) => <span key={group.name} className="font-mono text-[0.8125rem]">{group.name}: {group.servers.join(', ')} <span className="font-sans text-muted-foreground">({t(`network.dns.sources.${group.source}`)})</span></span>) : <span className="text-muted-foreground">{t('network.values.none')}</span>}</dd>
            <dt className="text-muted-foreground">{t('network.dns.probe')}</dt>
            <dd><ProbeText probe={dns.data?.probe} /></dd>
            <dt className="text-muted-foreground">{t('network.dns.records')}</dt>
            <dd>{t('network.dns.recordCount', { count: dns.data?.staticRecords.length ?? 0 })}</dd>
          </dl>
        </Card>
        <Card title={t('network.overview.wifiTitle')} action={<Button size="xs" variant="link" onClick={() => onTab('wifi')}>{t('network.overview.wifiLink')}</Button>}>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">{t('network.wifi.mode')}</dt>
            <dd>{wifi.mode === 'off' ? t('network.overview.wifiOff') : t(wifi.mode === 'ap' ? 'network.wifi.modes.ap' : 'network.wifi.modes.client')}</dd>
            {wifi.mode === 'ap' ? (
              <>
                <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className={wifi.apRunning ? 'text-success' : 'text-destructive'}>{t(wifi.apRunning ? 'network.status.apRunning' : 'network.status.apDown')}</dd>
                <dt className="text-muted-foreground">SSID</dt><dd>{wifi.apSsid || '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.clients')}</dt><dd>{wifi.stations}</dd>
              </>
            ) : wifi.mode === 'client' ? (
              <>
                <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className={wifi.association ? 'text-success' : 'text-warning'}>{t(wifi.association ? 'network.status.connected' : 'network.status.notConnected')}</dd>
                <dt className="text-muted-foreground">SSID</dt><dd>{wifi.association?.ssid ?? '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.signal')}</dt><dd>{wifi.association?.rssiDbm !== undefined ? `${wifi.association.rssiDbm} dBm` : '—'}</dd>
                <dt className="text-muted-foreground">{t('network.fields.address')}</dt><dd className="font-mono">{radio ? view.ifaces[radio].obs?.address ?? '—' : '—'}</dd>
              </>
            ) : null}
          </dl>
        </Card>
        <Card title={t('network.overview.tunnels')} aside={String(tunnels.length)}>
          {tunnels.length ? tunnels.map((name) => {
            const note = text.note(status(name))
            return (
              <div key={name} className="grid">
                <Row name={name} kind={kindLabel(view.ifaces[name])} status={status(name)} onClick={() => void navigate({ to: '/network', search: { tab: 'wireguard', tunnel: name } })} />
                <span className="ps-6 text-xs text-muted-foreground">{[view.ifaces[name].obs?.address, note].filter(Boolean).join(' · ')}</span>
              </div>
            )
          }) : <Empty>{t('network.overview.noTunnels')}</Empty>}
        </Card>
      </div>

      <div className="rounded-xl border bg-card">
        <button type="button" className="flex w-full items-center gap-2 p-4 text-left text-sm font-semibold" aria-expanded={showRoutes} onClick={() => setShowRoutes(!showRoutes)}>
          {showRoutes ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
          {exit ? t('network.overview.routesTitle', { count: routes.length, exit: exit.interface }) : t('network.overview.routesTitleNone', { count: routes.length })}
        </button>
        {showRoutes ? (
          <div className="border-t">
            <DataTable<RouteRow>
              rows={shownRoutes}
              rowKey={(route) => `${route.interface}-${route.destination}-${route.via ?? ''}-${route.metric ?? ''}`}
              empty={t('network.overview.noRoutes')}
              columns={[
                {
                  id: 'destination',
                  header: t('network.overview.columns.destination'),
                  cell: (route) => route.prefix === 0
                    ? <span className="flex items-center gap-2">{t('network.overview.default')}{route.role ? <Badge variant={route.role === 'effective' ? 'default' : 'outline'}>{t(`network.overview.roles.${route.role}`)}</Badge> : null}</span>
                    : <span className="font-mono">{route.destination}</span>,
                },
                { id: 'via', header: t('network.overview.columns.via'), cell: (route) => <span className="font-mono">{route.via ?? t('network.values.onLink')}</span> },
                { id: 'interface', header: t('network.overview.columns.interface'), cell: (route) => <button type="button" className="font-mono text-primary hover:underline" onClick={() => open(route.interface)}>{route.interface}</button> },
                { id: 'metric', header: t('network.overview.columns.metric'), cell: (route) => route.metric ?? '—' },
                { id: 'source', header: t('network.overview.columns.source'), cell: (route) => t(`network.overview.sources.${route.source}`) },
              ]}
            />
            {routes.length > ROUTES_FOLDED && !allRoutes ? (
              <div className="border-t px-4 py-2"><Button size="xs" variant="link" onClick={() => setAllRoutes(true)}>{t('network.overview.showAllRoutes', { count: routes.length })}</Button></div>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  )
}

/// Routes the table shows before it is unfolded.
const ROUTES_FOLDED = 20

function Node({ tone, icon, title, value, onClick, faint }: { tone: StatusTone; icon: ReactNode; title: string; value: ReactNode; onClick?: () => void; faint?: boolean }) {
  const body = (
    <>
      <span className={cn('flex items-center gap-1.5 text-xs text-muted-foreground [&_svg]:size-3.5')}>{icon}{title}</span>
      <strong className={cn('text-sm font-semibold', TEXT[tone])}>{value}</strong>
    </>
  )
  return onClick
    ? <button type="button" onClick={onClick} className={cn('grid gap-0.5 rounded-lg border p-3 text-left hover:border-primary/50', faint && 'opacity-70')}>{body}</button>
    : <div className={cn('grid gap-0.5 rounded-lg border p-3', faint && 'opacity-70')}>{body}</div>
}

function Wire({ tone, label }: { tone: StatusTone; label?: string }) {
  return (
    <div className="grid min-w-10 justify-items-center gap-1">
      {label ? <span className="font-mono text-[0.6875rem] text-muted-foreground">{label}</span> : null}
      <span className={cn('h-0.5 w-full rounded-full', tone === 'success' ? 'bg-success' : tone === 'warning' ? 'bg-warning' : tone === 'danger' ? 'bg-destructive' : 'bg-border')} />
    </div>
  )
}

function Card({ title, aside, action, children }: { title: string; aside?: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="grid content-start gap-3 rounded-xl border bg-card p-4">
      <div className="flex items-center gap-2"><h2 className="text-sm font-semibold">{title}</h2>{aside ? <span className="text-sm text-muted-foreground">{aside}</span> : null}<span className="flex-1" />{action}</div>
      {children}
    </section>
  )
}

function Row({ name, kind, status, onClick }: { name: string; kind: string; status: IfaceStatus; onClick: () => void }) {
  const text = useStatusText()
  return (
    <button type="button" onClick={onClick} className="flex items-center gap-2 rounded-md px-1 py-1 text-left text-sm hover:bg-muted">
      <Led tone={status.tone} /><span className="font-mono font-medium">{name}</span><span className="text-xs text-muted-foreground">{kind}</span>
      <span className={cn('ms-auto', TEXT[status.tone])}>{text.label(status)}</span>
    </button>
  )
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="py-2 text-sm text-muted-foreground">{children}</p>
}
