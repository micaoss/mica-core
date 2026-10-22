import { useCallback, useState } from 'react'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ChevronLeft, Play, RotateCw, Square } from 'lucide-react'
import { api } from '@/shared/lib/http'
import type { AppLog, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { Page, PageHeader, PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { RowItem, RowList } from '@/shared/components/row-item'
import { StatusBadge } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { Button, buttonVariants } from '@/shared/components/ui/button'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { appRoute, appState, appTone, useApps } from './apps'
import { PrivilegesList } from './privileges-asked'

type Verb = 'start' | 'stop' | 'restart'
type Flow = 'rollback' | 'check' | 'update'

/// One app: its version and where it came from, its units and their verbs,
/// its update, rollback and removal, and its log.
export function AppDetailPage() {
  const { name } = useParams({ from: '/services_/apps/$name' })
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const apps = useApps()
  const [task, setTask] = useState<string>()
  const app = apps.data?.apps.find((entry) => entry.name === name)
  const log = useQuery({
    queryKey: ['apps', 'log', name],
    queryFn: () => api<AppLog>(`${appRoute(name)}/logs`),
    enabled: app !== undefined,
  })

  const invalidate = useCallback(() => void queryClient.invalidateQueries({ queryKey: ['apps'] }), [queryClient])
  const act = useMutationFeedback<void, Verb>({
    mutationFn: (verb) => api<void>(`${appRoute(name)}/${verb}`, { method: 'POST' }),
    success: (_data, verb) => t(`services.units.acted.${verb}`, { name }),
    failure: name,
    onSuccess: invalidate,
  })
  const flow = useMutationFeedback<TaskAccepted, Flow>({
    mutationFn: (step) => api<TaskAccepted>(`${appRoute(name)}/${step}`, { method: 'POST' }),
    success: (_data, step) => t(`services.apps.flows.${step}`, { name }),
    failure: name,
    onSuccess: (accepted) => {
      setTask(accepted.taskId)
      invalidate()
    },
  })
  const remove = (purge: boolean) => api<TaskAccepted>(`${appRoute(name)}${purge ? '?purge=true' : ''}`, { method: 'DELETE' }).then(() => {
    invalidate()
    void navigate({ to: '/services' })
  })

  const back = <Link to="/services" className={buttonVariants({ variant: 'outline', size: 'sm' })}><ChevronLeft aria-hidden="true" />{t('services.title')}</Link>

  if (apps.isPending) {
    return <Page><PageHeader title={name} back={back} /><Spinner /></Page>
  }
  if (!app) {
    return (
      <Page>
        <PageHeader title={name} back={back} />
        {apps.error ? <Callout tone="danger" title={failureDetail(apps.error, t('common.requestFailed'))} /> : null}
        <Panel><p className="py-6 text-center text-sm text-muted-foreground">{t('services.unknown', { id: name })}</p></Panel>
      </Page>
    )
  }
  const busy = flow.isPending || act.isPending
  const rows = [
    { id: 'version', title: t('services.apps.version'), value: app.version },
    { id: 'previous', title: t('services.apps.previous'), value: app.previous ?? t('services.apps.none') },
    { id: 'source', title: t('services.apps.source'), value: t(`services.apps.sources.${app.source}`, { defaultValue: app.source }) },
    { id: 'installed', title: t('services.apps.installedAt'), value: app.installedAt },
    { id: 'sha256', title: 'sha256', value: app.sha256 },
  ]
  const privileges = app.privileges
  if (privileges) {
    rows.push({
      id: 'runsAs',
      title: t('services.apps.runsAs'),
      value: privileges.user === 'root' ? 'root' : t('services.apps.ownUser', { account: privileges.account, uid: privileges.uid }),
    })
    if (privileges.capabilities.length > 0) rows.push({ id: 'capabilities', title: t('services.apps.capabilities'), value: privileges.capabilities.join(', ') })
    if (privileges.devices.length > 0) rows.push({ id: 'devices', title: t('services.apps.devices'), value: privileges.devices.join(', ') })
  }

  return (
    <Page>
      <PageHeader
        title={name}
        description={app.description || undefined}
        back={back}
        action={<StatusBadge tone={appTone(app)}>{t(`services.apps.states.${appState(app)}`, { defaultValue: appState(app) })}</StatusBadge>}
      />

      {privileges?.user === 'root' ? <Callout tone="warning" title={t('services.apps.root')}>{t('services.apps.rootCopy')}</Callout> : null}
      {app.last?.outcome === 'failed' ? (
        <Callout tone="warning" title={t('services.apps.lastFailed', { action: app.last.action })}>{app.last.message}</Callout>
      ) : null}
      <TaskProgress taskId={task} onFinished={invalidate} />

      <PageSection title={t('services.apps.versionTitle')} description={t('services.apps.versionCopy')}>
        <div className="grid gap-3">
        <Panel contentClassName="gap-0 p-0">
          <RowList>
            {rows.map((row) => (
              <RowItem key={row.id} title={row.title} actions={<span className="font-mono text-sm break-all text-muted-foreground">{row.value}</span>} />
            ))}
          </RowList>
        </Panel>
        <span className="flex flex-wrap justify-end gap-2">
          <ConfirmDialog
            trigger={<Button type="button" variant="outline" disabled={!app.previous || busy}>{t('services.apps.rollback', { version: app.previous ?? '' })}</Button>}
            tone="neutral"
            title={t('services.apps.rollback', { version: app.previous ?? '' })}
            description={t('services.apps.rollbackCopy', { name, version: app.previous ?? '' })}
            confirmLabel={t('services.apps.rollback', { version: app.previous ?? '' })}
            success={t('services.apps.flows.rollback', { name })}
            failure={name}
            onConfirm={() => api<TaskAccepted>(`${appRoute(name)}/rollback`, { method: 'POST' }).then((accepted) => { setTask(accepted.taskId); invalidate() })}
          />
          <ConfirmDialog
            trigger={<Button type="button" variant="destructive" disabled={busy}>{t('services.apps.remove', { name })}</Button>}
            title={t('services.apps.remove', { name })}
            description={t('services.apps.removeCopy', { name })}
            confirmLabel={t('services.apps.remove', { name })}
            success={t('services.apps.removed', { name })}
            failure={t('services.apps.remove', { name })}
            onConfirm={() => remove(false)}
          />
          <ConfirmDialog
            trigger={<Button type="button" variant="destructive" disabled={busy}>{t('services.apps.purge')}</Button>}
            title={t('services.apps.purge')}
            description={t('services.apps.purgeCopy', { name })}
            confirmLabel={t('services.apps.purge')}
            success={t('services.apps.removed', { name })}
            failure={t('services.apps.purge')}
            onConfirm={() => remove(true)}
          />
        </span>
        </div>
      </PageSection>

      <PageSection title={t('services.apps.units')} description={t('services.apps.unitsCopy')}>
        <Panel contentClassName="gap-3">
          {app.units.length === 0 ? <p className="text-sm text-muted-foreground">{t('services.apps.noUnits')}</p> : (
            <RowList>
              {app.units.map((unit) => (
                <RowItem
                  key={unit.name}
                  title={<span className="font-mono">{unit.name}</span>}
                  description={t(unit.enabled ? 'services.apps.atBoot' : 'services.apps.notAtBoot')}
                  actions={<StatusBadge tone={unit.active === 'active' ? 'success' : unit.active === 'failed' ? 'warning' : 'neutral'}>{unit.active ?? t('common.states.unknown')}</StatusBadge>}
                />
              ))}
            </RowList>
          )}
          <span className="flex flex-wrap justify-end gap-2">
            <Button type="button" size="sm" variant="outline" disabled={app.units.length === 0 || busy} onClick={() => act.mutate('start')}><Play />{t('services.units.start')}</Button>
            <Button type="button" size="sm" variant="outline" disabled={app.units.length === 0 || busy} onClick={() => act.mutate('restart')}><RotateCw />{t('services.units.restart')}</Button>
            <Button type="button" size="sm" variant="outline" disabled={app.units.length === 0 || busy} onClick={() => act.mutate('stop')}><Square />{t('services.units.stop')}</Button>
          </span>
        </Panel>
      </PageSection>

      {app.update ? (
        <PageSection title={t('services.apps.update')} description={t('services.apps.updateCopy')}>
          <Panel contentClassName="gap-3">
            <p className="font-mono text-sm break-all text-muted-foreground">{app.update.url}</p>
            <p className="text-sm">
              {app.update.available
                ? t('services.apps.available', { version: app.update.available })
                : app.update.checkedAt ? t('services.apps.current', { at: app.update.checkedAt }) : t('services.apps.notChecked')}
            </p>
            {app.update.error ? <Callout tone="warning" title={app.update.error} /> : null}
            <span className="flex flex-wrap justify-end gap-2">
              <Button type="button" size="sm" variant="outline" disabled={busy} onClick={() => flow.mutate('check')}>{t('services.apps.check')}</Button>
              {app.update.available && app.update.privileges ? (
                <ConfirmDialog
                  trigger={<Button type="button" size="sm" disabled={busy}>{t('services.apps.updateTo', { version: app.update.available })}</Button>}
                  tone={app.update.privileges.root ? 'danger' : 'neutral'}
                  title={t('services.apps.privileges.updateTitle', { version: app.update.available })}
                  description={(
                    <span className="grid gap-2">
                      <span>{t('services.apps.privileges.updateCopy')}</span>
                      <PrivilegesList asked={app.update.privileges} />
                    </span>
                  )}
                  confirmLabel={t('services.apps.privileges.acceptUpdate')}
                  success={t('services.apps.flows.update', { name })}
                  failure={name}
                  onConfirm={async () => {
                    const accepted = await api<TaskAccepted>(`${appRoute(name)}/update?acceptPrivileges=true`, { method: 'POST' })
                    setTask(accepted.taskId)
                    invalidate()
                  }}
                />
              ) : (
                <Button type="button" size="sm" disabled={!app.update.available || busy} onClick={() => flow.mutate('update')}>{t('services.apps.updateTo', { version: app.update.available ?? '' })}</Button>
              )}
            </span>
            {app.update.available && app.update.privileges ? (
              <Callout tone="warning" title={t('services.apps.privileges.updateTitle', { version: app.update.available })}>
                <PrivilegesList asked={app.update.privileges} />
              </Callout>
            ) : null}
          </Panel>
        </PageSection>
      ) : null}

      <PageSection title={t('services.units.log')} description={t('services.apps.logCopy')}>
        <Panel contentClassName="gap-3">
          <span className="flex justify-end">
            <Button type="button" size="sm" variant="outline" disabled={log.isFetching} onClick={() => void log.refetch()}>{t('common.actions.refresh')}</Button>
          </span>
          {log.isPending ? <Spinner /> : log.error ? (
            <Callout tone="danger" title={failureDetail(log.error, t('common.requestFailed'))} />
          ) : (
            <>
              {log.data.units.map((unit) => (
                <div key={unit.unit} className="grid gap-1">
                  <span className="font-mono text-xs text-muted-foreground">{unit.unit}</span>
                  {unit.available === false
                    ? <p className="text-sm text-muted-foreground">{unit.detail ?? t('services.units.logUnavailable')}</p>
                    : <pre className="max-h-72 overflow-auto font-mono text-xs whitespace-pre-wrap break-all">{unit.lines?.join('\n') || t('services.units.logEmpty')}</pre>}
                </div>
              ))}
              {log.data.firstRun ? (
                <div className="grid gap-1">
                  <span className="font-mono text-xs text-muted-foreground">{t('services.apps.firstRun')}</span>
                  <pre className="max-h-72 overflow-auto font-mono text-xs whitespace-pre-wrap break-all">{log.data.firstRun.join('\n') || t('services.units.logEmpty')}</pre>
                </div>
              ) : null}
            </>
          )}
        </Panel>
      </PageSection>
    </Page>
  )
}
