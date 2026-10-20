import { useState } from 'react'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ChevronLeft, Play, RotateCw, Square } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, UnitLog, UserUnit } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { Page, PageHeader, PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button, buttonVariants } from '@/shared/components/ui/button'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { UnitFields } from './units-panel'
import { UNITS_SETTINGS, unitRoute, unitTone, useDeclaredUnits, useUnits } from './units'

type Verb = 'start' | 'stop' | 'restart'

/// One of the user's services: its state and the init's verbs, its text to
/// edit when it is declared, and its log.
export function UnitDetailPage() {
  const { name } = useParams({ from: '/services_/units/$name' })
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const units = useUnits()
  const declared = useDeclaredUnits()
  const [draft, setDraft] = useState<UserUnit>()
  const entry = units.data?.units.find((unit) => unit.name === name)
  const unit = declared.data?.[name]
  const installed = entry?.installed === true
  const log = useQuery({
    queryKey: ['units', 'log', name],
    queryFn: () => api<UnitLog>(`${unitRoute(name)}/logs`),
    enabled: installed,
  })

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['units'] })
    void queryClient.invalidateQueries({ queryKey: ['settings', UNITS_SETTINGS] })
  }
  const write = useMutationFeedback<TaskAccepted, UserUnit>({
    mutationFn: (next) => api<TaskAccepted>(unitRoute(name), json('PUT', next)),
    success: () => t('services.units.saved', { name }),
    failure: name,
    onSuccess: () => {
      setDraft(undefined)
      invalidate()
    },
  })
  const act = useMutationFeedback<void, Verb>({
    mutationFn: (action) => api<void>(`${unitRoute(name)}/${action}`, { method: 'POST' }),
    success: (_data, action) => t(`services.units.acted.${action}`, { name }),
    failure: name,
    onSuccess: invalidate,
  })
  const remove = () => api<TaskAccepted>(unitRoute(name), { method: 'DELETE' }).then(() => {
    invalidate()
    void navigate({ to: '/services' })
  })

  const back = <Link to="/services" className={buttonVariants({ variant: 'outline', size: 'sm' })}><ChevronLeft aria-hidden="true" />{t('services.title')}</Link>

  if (units.isPending || declared.isPending) {
    return <Page><PageHeader title={name} back={back} /><Spinner /></Page>
  }
  if (!entry && !unit) {
    return (
      <Page>
        <PageHeader title={name} back={back} />
        {units.error ? <Callout tone="danger" title={failureDetail(units.error, t('common.requestFailed'))} /> : null}
        <Panel><p className="py-6 text-center text-sm text-muted-foreground">{t('services.unknown', { id: name })}</p></Panel>
      </Page>
    )
  }

  const managed = entry?.managed ?? unit?.kind === units.data?.init
  const editing = draft ?? unit

  return (
    <Page>
      <PageHeader
        title={name}
        description={t(unit ? 'services.units.declaredCopy' : 'services.units.byHandCopy')}
        back={back}
        action={unit && managed ? (
          <>
            <span className="text-sm font-medium">{t(unit.enabled ? 'common.states.enabled' : 'common.states.disabled')}</span>
            <Switch
              checked={unit.enabled}
              onCheckedChange={(enabled) => write.mutate({ ...unit, enabled })}
              disabled={write.isPending}
              aria-label={name}
            />
          </>
        ) : undefined}
      />

      {entry?.refused ? <Callout tone="warning" title={t('services.units.refused', { detail: entry.refused })} /> : null}

      <PageSection title={t('services.units.state')} description={t('services.units.stateCopy')}>
        <Panel contentClassName="flex flex-wrap items-center justify-between gap-3">
          <StatusBadge tone={unitTone(entry)}>{installed ? entry?.active ?? t('common.states.unknown') : t('services.units.notInstalled')}</StatusBadge>
          <span className="flex gap-2">
            <Button type="button" size="sm" variant="outline" disabled={!installed || act.isPending} onClick={() => act.mutate('start')}><Play />{t('services.units.start')}</Button>
            <Button type="button" size="sm" variant="outline" disabled={!installed || act.isPending} onClick={() => act.mutate('restart')}><RotateCw />{t('services.units.restart')}</Button>
            <Button type="button" size="sm" variant="outline" disabled={!installed || act.isPending} onClick={() => act.mutate('stop')}><Square />{t('services.units.stop')}</Button>
          </span>
        </Panel>
      </PageSection>

      {editing ? (
        <PageSection title={t('services.units.file')} description={t('services.units.editCopy')}>
          <Panel contentClassName="gap-3">
            <form
              className="grid gap-3"
              onSubmit={(event) => {
                event.preventDefault()
                write.mutate(editing)
              }}
            >
              <UnitFields value={editing} onChange={setDraft} />
              <span className="flex flex-wrap justify-end gap-2">
                <ConfirmDialog
                  trigger={<Button type="button" variant="destructive">{t('services.units.remove', { name })}</Button>}
                  title={t('services.units.remove', { name })}
                  description={t('services.units.removeCopy', { name })}
                  confirmLabel={t('services.units.remove', { name })}
                  success={t('services.units.removed', { name })}
                  failure={t('services.units.remove', { name })}
                  onConfirm={remove}
                />
                <Button type="submit" disabled={write.isPending || draft === undefined}>{t('common.actions.save')}</Button>
              </span>
            </form>
          </Panel>
        </PageSection>
      ) : null}

      <PageSection title={t('services.units.log')} description={t('services.units.logCopy')}>
        <Panel>
          <span className="flex flex-wrap items-center justify-between gap-2">
            <span className="font-mono text-xs text-muted-foreground">{log.data?.source}</span>
            <Button type="button" size="sm" variant="outline" disabled={!installed || log.isFetching} onClick={() => void log.refetch()}>{t('common.actions.refresh')}</Button>
          </span>
          {!installed ? (
            <p className="text-sm text-muted-foreground">{t('services.units.notInstalled')}</p>
          ) : log.isPending ? <Spinner /> : log.error ? (
            <Callout tone="danger" title={failureDetail(log.error, t('common.requestFailed'))} />
          ) : log.data.available === false ? (
            <Callout tone="neutral" title={log.data.detail ?? t('services.units.logUnavailable')} />
          ) : (
            <>
              {log.data.truncated ? <p className="text-xs text-muted-foreground">{t('services.units.logTruncated')}</p> : null}
              <pre className="max-h-96 overflow-auto font-mono text-xs whitespace-pre-wrap break-all">{log.data.lines?.join('\n') || t('services.units.logEmpty')}</pre>
            </>
          )}
        </Panel>
      </PageSection>
    </Page>
  )
}
