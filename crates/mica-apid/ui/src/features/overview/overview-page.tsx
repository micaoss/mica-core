import { Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api } from '@/shared/lib/http'
import type { Health, ObservedNetworkState, SystemInformation, TaskRecord, TimeStatus } from '@/lib/types'
import { selectUplink } from '@/lib/network'
import { Callout } from '@/shared/components/callout'
import { DataTable } from '@/shared/components/data-table'
import { MetricCard } from '@/shared/components/metric-card'
import { Page, PageHeader } from '@/shared/components/page'
import { CollectionPanel } from '@/shared/components/panel'
import { RowItem, RowList } from '@/shared/components/row-item'
import { StatusBadge } from '@/shared/components/status-badge'
import { buttonVariants } from '@/shared/components/ui/button'
import { formatAge, formatKnownState } from '@/i18n/format'
import { freshnessLabel } from '@/features/shell/connection'

interface UpdateLifecycle {
  lifecycle?: { state?: string; available?: { name?: string; version?: string } }
}

export function OverviewPage() {
  const { t } = useTranslation()
  const health = useQuery({ queryKey: ['health'], queryFn: () => api<Health>('/api/v1/health'), refetchInterval: 15_000 })
  const information = useQuery({ queryKey: ['system-information'], queryFn: () => api<SystemInformation>('/api/v1/system/info'), retry: false })
  // The observed view and not `/api/v1/network`: the declared map says what an
  // integrator asked for, and the card is here to say what the device actually
  // reached the world through.
  const network = useQuery({ queryKey: ['network-status'], queryFn: () => api<ObservedNetworkState>('/api/v1/network/status'), refetchInterval: 10_000 })
  const hostname = useQuery({ queryKey: ['settings', 'hostname'], queryFn: () => api<string>('/api/v1/settings/hostname') })
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: () => api<TaskRecord[]>('/api/v1/tasks'), refetchInterval: 5_000 })
  const update = useQuery({ queryKey: ['update-state'], queryFn: () => api<UpdateLifecycle>('/api/v1/update'), retry: false })
  const time = useQuery({ queryKey: ['time-status'], queryFn: () => api<TimeStatus>('/api/v1/time/status'), retry: false })

  const healthy = health.data?.micad === 'ok' && health.data?.apid === 'ok'
  const healthLabel = healthy ? t('overview.allResponding') : health.isPending ? t('overview.checking') : t('overview.unavailable')
  const queryError = health.error ?? network.error ?? tasks.error

  const available = update.data?.lifecycle?.available
  const clockAdrift = time.data !== undefined && time.data.synchronized === false
  const attention = [
    available ? {
      id: 'update',
      tone: 'neutral' as const,
      tag: t('overview.attention.updateTag'),
      title: t('overview.attention.updateTitle', { version: available.version ?? available.name ?? '' }),
      copy: t('overview.attention.updateCopy'),
      action: t('overview.attention.review'),
      hash: 'update',
    } : undefined,
    clockAdrift ? {
      id: 'time',
      tone: 'warning' as const,
      tag: t('overview.attention.timeTag'),
      title: t('overview.attention.timeTitle'),
      copy: t('overview.attention.timeCopy'),
      action: t('overview.attention.configure'),
      hash: 'time',
    } : undefined,
  ].filter((item) => item !== undefined)

  const uplink = selectUplink(network.data)
  const machineId = information.data?.machineId
  const uptime = information.data?.uptime
  const release = information.data?.release
  const deployment = information.data?.deployment
  const board = information.data?.board
  const kernel = information.data?.kernel
  const device = [
    { id: 'machineId', label: t('overview.device.machineId'), value: machineId?.available ? machineId.id : undefined },
    { id: 'board', label: t('overview.device.board'), value: board?.available ? board.model : undefined },
    { id: 'software', label: t('overview.device.software'), value: release?.available ? release.version ?? release.prettyName : undefined },
    { id: 'deployment', label: t('overview.device.deployment'), value: deployment?.available ? deployment.version : undefined },
    { id: 'kernel', label: t('overview.device.kernel'), value: kernel?.available ? kernel.release : undefined },
  ]

  return (
    <Page>
      <PageHeader
        title={t('overview.title')}
        action={
          <>
            <StatusBadge tone={healthy ? 'success' : health.isPending ? 'warning' : 'danger'}>{healthLabel}</StatusBadge>
            <span className="text-sm whitespace-nowrap text-muted-foreground">{freshnessLabel(health.dataUpdatedAt, t)}</span>
          </>
        }
      />
      {queryError ? <Callout tone="danger" title={t('common.requestFailed')} /> : null}
      {attention.length > 0 ? (
        <section className="flex flex-col gap-2">
          <p className="text-sm text-muted-foreground">{t('overview.attention.title')} · {attention.length}</p>
          <CollectionPanel>
            <RowList>
              {attention.map((item) => (
                <RowItem
                  key={item.id}
                  media={<StatusBadge tone={item.tone}>{item.tag}</StatusBadge>}
                  title={item.title}
                  description={item.copy}
                  actions={<Link to="/system" hash={item.hash} className={buttonVariants({ variant: 'outline', size: 'sm' })}>{item.action}</Link>}
                />
              ))}
            </RowList>
          </CollectionPanel>
        </section>
      ) : null}
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <MetricCard
          label={t('overview.systemHealth')}
          value={healthy ? t('common.states.healthy') : t('common.states.unknown')}
          caption={t('overview.systemHealthCopy')}
          render={(card) => <Link to="/services" className="contents">{card}</Link>}
        />
        <MetricCard
          label={t('overview.identity')}
          mono
          value={hostname.data ?? t('common.notAvailable')}
          caption={machineId?.available && machineId.id ? t('overview.machineId', { id: machineId.id.slice(0, 8) }) : t('common.notAvailable')}
          render={(card) => <Link to="/system" hash="information" className="contents">{card}</Link>}
        />
        <MetricCard
          label={t('overview.network')}
          mono
          value={uplink ? (uplink.address ? `${uplink.name} · ${uplink.address}` : uplink.name) : t('common.notAvailable')}
          caption={uplink?.operationalState ? formatKnownState(uplink.operationalState, t) : t('overview.liveUnavailable')}
          render={(card) => <Link to="/network" className="contents">{card}</Link>}
        />
        <MetricCard
          label={t('overview.deviceUptime')}
          mono
          value={uptime?.available && uptime.seconds !== undefined ? formatUptime(uptime.seconds, t) : t('common.notAvailable')}
          caption={t('overview.readFromMicad')}
          render={(card) => <Link to="/system" hash="information" className="contents">{card}</Link>}
        />
      </div>
      <CollectionPanel
        title={t('overview.device.title')}
        action={
          <Link to="/system" hash="information" className={buttonVariants({ variant: 'outline', size: 'sm' })}>
            {t('overview.device.open')}
          </Link>
        }
      >
        <RowList>
          {device.map((fact) => (
            <RowItem
              key={fact.id}
              title={fact.label}
              description={<span className="font-mono text-[0.8125rem]">{fact.value ?? t('common.notAvailable')}</span>}
            />
          ))}
        </RowList>
      </CollectionPanel>
      <CollectionPanel
        title={t('overview.recentTasks')}
        action={<span className="text-sm whitespace-nowrap text-muted-foreground">{freshnessLabel(tasks.dataUpdatedAt, t)}</span>}
      >
        <DataTable<TaskRecord>
          rows={tasks.data?.slice(-6).reverse()}
          rowKey={(task) => task.id}
          isPending={tasks.isPending}
          empty={t('overview.noTasks')}
          columns={[
            { id: 'action', header: t('overview.tasks.action'), cell: (task) => task.operation },
            { id: 'target', header: t('overview.tasks.target'), cell: (task) => <span className="font-mono text-[0.8125rem]">{task.dotPath}</span> },
            { id: 'phase', header: t('overview.tasks.phase'), cell: (task) => {
              const phase = task.status === 'finished' ? task.outcome ?? 'finished' : task.status
              return <StatusBadge tone={task.outcome === 'succeeded' ? 'success' : task.outcome === 'failed' ? 'danger' : 'warning'}>{formatKnownState(phase, t)}</StatusBadge>
            } },
            { id: 'result', header: t('overview.tasks.result'), cell: (task) => task.message ?? t('overview.tasks.noDetail', { source: task.source }) },
            { id: 'time', header: t('overview.tasks.time'), align: 'end', cell: (task) => formatAge(Date.now() - Date.parse(task.enqueuedAt), t) },
          ]}
        />
      </CollectionPanel>
    </Page>
  )
}

function formatUptime(seconds: number, t: ReturnType<typeof useTranslation>['t']) {
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  return days
    ? t('overview.uptime.daysHours', { days, hours })
    : t('overview.uptime.hoursMinutes', { hours, minutes: Math.floor((seconds % 3600) / 60) })
}
