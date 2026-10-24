import { useState, type ReactNode } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ChevronDown, ChevronRight, Globe } from 'lucide-react'
import { cn } from '@/shared/lib/utils'
import type { StatusTone } from '@/shared/components/status-badge'
import { DataTable } from '@/shared/components/data-table'
import { Badge } from '@/shared/components/ui/badge'
import { Button } from '@/shared/components/ui/button'
import { dnsLinks, ProbeText, useDns } from './dns-tab'
import { bridgeOf, usesInternet, vlansOn } from './model'
import { Led, Tag, useKindLabel, useStatusText, useWifiText } from './parts'
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
  const wifiText = useWifiText()
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
  const wifi = view.wifi
  const lanOnly = (name: string) => !usesInternet(view.ifaces[name].entry) ? <Tag>{t('network.tags.lanOnly')}</Tag> : null
  const dns = useDns()
  const globalDns = dns.data?.mode === 'global'
  const dnsGroups = dnsLinks(data, dns.data).filter((link) => link.servers.length > 0)

  return (
    <div className="grid grid-cols-1 gap-6">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <div className="grid gap-0.5">
          <span className="flex items-center gap-1.5 text-[0.8125rem] text-muted-foreground"><Globe className="size-3.5" aria-hidden="true" />{t('network.overview.internet')}</span>
          <span className="flex items-center gap-1.5 text-base font-semibold"><Led tone={exit ? 'success' : 'danger'} />{t(exit ? 'network.overview.online' : 'network.overview.offline')}</span>
        </div>
        <Wire tone={exit ? 'success' : 'danger'} label={exit ? t('network.overview.via', { gateway: exit.gateway ?? '—' }) : t('network.overview.noDefault')} />
        <div className="flex flex-wrap gap-2">
          {uplink ? [uplink, ...standby].map((name) => (
            <Chip
              key={name}
              effective={exit?.interface === name}
              title={t(name === uplink ? 'network.overview.uplink' : 'network.overview.standby', { name })}
              value={view.ifaces[name].obs?.address ?? text.label(status(name))}
              onClick={() => open(name)}
            />
          )) : <Chip title={t('network.overview.noUplink')} value="—" />}
        </div>
      </div>

      <div className="grid gap-6 lg:grid-cols-[2fr_1fr]">
        <Card title={t('network.roles.phys')} aside={t('network.overview.portsLinked', { count: physical.length, up: physical.filter((name) => view.ifaces[name].obs?.carrier).length })}>
          {physical.length ? (
            <div className="grid gap-x-6 gap-y-5 sm:grid-cols-2 xl:grid-cols-3">
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
                  <div key={name} className="grid content-start gap-1 text-[0.8125rem]">
                    <span className="flex flex-wrap items-center gap-2"><button type="button" className="font-mono text-sm font-semibold hover:text-primary hover:underline" onClick={() => open(name)}>{name}</button>{lanOnly(name)}</span>
                    <span className="flex items-center gap-1.5"><Led tone={status(name).tone} />{text.label(status(name))}</span>
                    <span className="text-muted-foreground">{uses.length ? uses.join(' · ') : t('network.overview.useStandalone')}</span>
                    <span className="font-mono break-all text-muted-foreground">{address}</span>
                  </div>
                )
              })}
            </div>
          ) : <Empty>{t('network.overview.noPorts')}</Empty>}
        </Card>
        <Card title={t('network.overview.virtual')} aside={t('network.list.count', { count: lan.length })}>
          {lan.length ? lan.map((name) => <Row key={name} name={name} kind={kindLabel(view.ifaces[name])} status={status(name)} lanOnly={lanOnly(name)} onClick={() => open(name)} />) : <Empty>{t('network.overview.noVirtual')}</Empty>}
        </Card>
      </div>

      <div className="grid gap-6 lg:grid-cols-3">
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
                <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className="flex items-center gap-1.5"><Led tone={wifi.apRunning ? 'success' : 'danger'} />{t(wifi.apRunning ? 'network.status.apRunning' : 'network.status.apDown')}</dd>
                <dt className="text-muted-foreground">SSID</dt><dd>{wifi.apSsid || '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.channel')}</dt><dd>{wifi.apChannel ? wifiText.apChannel(wifi) : '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.clients')}</dt><dd>{wifi.stations}</dd>
              </>
            ) : wifi.mode === 'client' ? (
              <>
                <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className="flex items-center gap-1.5"><Led tone={wifi.association ? 'success' : 'warning'} />{t(wifi.association ? 'network.status.connected' : 'network.status.notConnected')}</dd>
                <dt className="text-muted-foreground">SSID</dt><dd>{wifi.association?.ssid ?? '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.signal')}</dt><dd>{wifi.association?.rssiDbm !== undefined ? wifiText.signal(wifi.association.rssiDbm) : '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.bssid')}</dt><dd className="font-mono">{wifi.association?.bssid ?? '—'}</dd>
                <dt className="text-muted-foreground">{t('network.wifi.linkRate')}</dt><dd>{wifi.association?.linkSpeedMbps ? t('network.wifi.linkRateValue', { mbps: wifi.association.linkSpeedMbps }) : '—'}</dd>
                <dt className="text-muted-foreground">{t('network.fields.address')}</dt><dd className="font-mono">{radio ? view.ifaces[radio].obs?.address ?? '—' : '—'}</dd>
                <dt className="text-muted-foreground">{t('network.fields.gateway')}</dt><dd className="font-mono">{radio ? view.ifaces[radio].obs?.lease?.router ?? '—' : '—'}</dd>
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

      <div className="rounded-xl bg-card">
        <button type="button" className="flex w-full items-center gap-2 p-5 text-left text-base font-semibold" aria-expanded={showRoutes} onClick={() => setShowRoutes(!showRoutes)}>
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

/// An uplink in the strip: the one the default route takes is filled, a
/// standby is muted.
function Chip({ title, value, effective, onClick }: { title: string; value: ReactNode; effective?: boolean; onClick?: () => void }) {
  const body = (
    <>
      <span className={cn('text-[0.8125rem]', effective ? 'text-primary-foreground/80' : 'text-muted-foreground')}>{title}</span>
      <strong className="font-mono text-sm font-semibold">{value}</strong>
    </>
  )
  const style = cn('grid gap-0.5 rounded-lg px-3 py-2 text-left', effective ? 'bg-primary text-primary-foreground' : 'bg-muted text-muted-foreground')
  return onClick
    ? <button type="button" onClick={onClick} className={cn(style, effective ? 'hover:bg-primary-hover' : 'hover:text-foreground')}>{body}</button>
    : <div className={style}>{body}</div>
}

function Wire({ tone, label }: { tone: StatusTone; label?: string }) {
  return (
    <div className="grid min-w-24 max-w-48 flex-1 justify-items-center gap-1">
      {label ? <span className="font-mono text-[0.8125rem] text-muted-foreground">{label}</span> : null}
      <span className={cn('h-px w-full', tone === 'success' ? 'bg-success' : tone === 'warning' ? 'bg-warning' : tone === 'danger' ? 'bg-destructive' : 'bg-border')} />
    </div>
  )
}

function Card({ title, aside, action, children }: { title: string; aside?: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="grid content-start gap-4 rounded-xl bg-card p-5">
      <div className="flex items-center gap-2"><h2 className="text-base font-semibold">{title}</h2>{aside ? <span className="text-[0.8125rem] text-muted-foreground">{aside}</span> : null}<span className="flex-1" />{action}</div>
      {children}
    </section>
  )
}

function Row({ name, kind, status, lanOnly, onClick }: { name: string; kind: string; status: IfaceStatus; lanOnly?: ReactNode; onClick: () => void }) {
  const text = useStatusText()
  return (
    <button type="button" onClick={onClick} className="flex items-center gap-2 rounded-md px-1 py-1 text-left text-sm hover:bg-muted">
      <span className="font-mono font-medium">{name}</span><span className="text-[0.8125rem] text-muted-foreground">{kind}</span>{lanOnly}
      <span className="ms-auto flex items-center gap-1.5"><Led tone={status.tone} />{text.label(status)}</span>
    </button>
  )
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="py-2 text-sm text-muted-foreground">{children}</p>
}
