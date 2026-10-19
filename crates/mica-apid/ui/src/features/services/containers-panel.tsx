import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Boxes, FileText, Play, RotateCw, Square, Trash2 } from 'lucide-react'
import { ApiError, api, json, type ApiProblem } from '@/shared/lib/http'
import type { ComposeCheck, ContainerOverview, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField } from '@/shared/components/form-field'
import { CollectionPanel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Dialog, DialogBody, DialogContent, DialogHeader, DialogTitle } from '@/shared/components/ui/dialog'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Textarea } from '@/shared/components/ui/textarea'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { projectRows, type Observed, type ProjectRow, type ServiceRow } from './containers'

type Verb = 'start' | 'stop' | 'restart'

/// What a verb or a log is sent to: the route under `/api/v1/`, and the name
/// the operator reads.
interface Target { route: string; label: string }

const PROJECTS = '/api/v1/container-projects'

function projectTarget(project: string): Target {
  return { route: `${PROJECTS}/${encodeURIComponent(project)}`, label: project }
}

function serviceTarget(project: string, service: string): Target {
  return { route: `${PROJECTS}/${encodeURIComponent(project)}/services/${encodeURIComponent(service)}`, label: `${project}/${service}` }
}

const TEMPLATE = 'services:\n  web:\n    image: docker.io/library/nginx:1\n    ports:\n      - "8080:80"\n    restart: always\n'

/// Running and not failing its health check is fine; anything else observed
/// needs a look.
function stateTone(row: Observed) {
  if (row.state === undefined) return 'neutral'
  return row.state === 'running' && row.health !== 'unhealthy' ? 'success' : 'warning'
}

/// The Compose projects this device declares, each with what the engine says
/// about its services.
///
/// Declared and observed are read from two places: the settings say what
/// should run, mica-containerd says what does. A project declared and not yet
/// taken, and one the daemon still holds that nobody declares, are both real
/// states and the panel says which it is.
export function ContainersPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<{ name: string; compose: string; creating: boolean }>()
  const [checked, setChecked] = useState<{ ok: boolean; count?: number; detail?: string; problems?: ApiProblem[] }>()
  const [checking, setChecking] = useState(false)
  const [logOf, setLogOf] = useState<Target>()
  const containers = useQuery({ queryKey: ['containers'], queryFn: () => api<ContainerOverview>('/api/v1/containers'), refetchInterval: 10_000 })
  const log = useQuery({
    queryKey: ['containers', 'log', logOf?.route],
    queryFn: () => api<{ log: string }>(`${logOf?.route}/logs?tail=200`),
    enabled: logOf !== undefined,
  })
  const projects = projectRows(containers.data)
  const docker = containers.data?.containerEngine === 'docker'

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['containers'] })
  }
  const act = useMutationFeedback<void, { target: Target; action: Verb }>({
    mutationFn: ({ target, action }) => api<void>(`${target.route}/${action}`, { method: 'POST' }),
    success: (_data, { target, action }) => t(`services.containers.acted.${action}`, { name: target.label }),
    failure: t('services.containers.title'),
    onSuccess: invalidate,
  })
  const remove = useMutationFeedback<TaskAccepted, Target>({
    mutationFn: (target) => api<TaskAccepted>(target.route, { method: 'DELETE' }),
    success: (_data, target) => t('services.containers.removed', { name: target.label }),
    failure: t('services.containers.title'),
    onSuccess: invalidate,
  })

  const edit = (next: typeof editing) => {
    setEditing(next)
    setChecked(undefined)
  }
  const check = () => {
    if (!editing) return
    setChecking(true)
    api<ComposeCheck>(`${projectTarget(editing.name.trim()).route}/check`, json('POST', { compose: editing.compose }))
      .then((answer) => setChecked({ ok: answer.ok, count: answer.containers?.length, detail: answer.detail, problems: answer.problems }))
      .catch((error: unknown) => setChecked({ ok: false, detail: failureDetail(error, t('common.requestFailed')), problems: error instanceof ApiError ? error.problems : undefined }))
      .finally(() => setChecking(false))
  }
  const save = () => {
    if (!editing) return Promise.resolve()
    return api<TaskAccepted>(projectTarget(editing.name.trim()).route, json('PUT', { compose: editing.compose }))
      .then(() => {
        setEditing(undefined)
        invalidate()
      })
      .catch((error: unknown) => {
        // A refused file keeps the dialog open with every problem beside
        // the text, as the check shows them.
        if (error instanceof ApiError && error.problems) setChecked({ ok: false, detail: error.message, problems: error.problems })
        throw error
      })
  }

  const verbs = (target: Target, disabled: boolean) => (
    <>
      <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.start', { name: target.label })} disabled={disabled} onClick={() => act.mutate({ target, action: 'start' })}><Play /></Button>
      <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.restart', { name: target.label })} disabled={disabled} onClick={() => act.mutate({ target, action: 'restart' })}><RotateCw /></Button>
      <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.stop', { name: target.label })} disabled={disabled} onClick={() => act.mutate({ target, action: 'stop' })}><Square /></Button>
    </>
  )
  const removal = (target: Target, copy: string, disabled: boolean) => (
    <ConfirmDialog
      trigger={<Button type="button" size="icon-sm" variant="destructive" aria-label={t('services.containers.remove', { name: target.label })} disabled={disabled}><Trash2 /></Button>}
      title={t('services.containers.remove', { name: target.label })}
      description={copy}
      confirmLabel={t('services.containers.remove', { name: target.label })}
      success={t('services.containers.removed', { name: target.label })}
      failure={t('services.containers.remove', { name: target.label })}
      onConfirm={() => remove.mutateAsync(target)}
    />
  )
  const state = (row: Observed) => (
    <span className="flex flex-wrap items-center gap-2">
      <StatusBadge tone={stateTone(row)}>
        {row.state ?? t('services.containers.notObserved')}{row.health ? ` · ${row.health}` : ''}
      </StatusBadge>
      {row.restarts ? <span className="text-muted-foreground text-xs">{t('services.containers.restartCount', { count: row.restarts })}</span> : null}
      {row.state === 'exited' || row.state === 'fatal' ? <span className="text-muted-foreground text-xs">{t('services.containers.exitCode', { code: row.exitCode ?? 0 })}</span> : null}
      {row.error ? <span className="text-muted-foreground text-xs break-all">{row.error}</span> : null}
    </span>
  )

  return (
    <div className="grid gap-3">
      {containers.error ? <Callout tone="danger" title={failureDetail(containers.error, t('common.requestFailed'))} /> : null}
      {/* Off is the default and mica-containerd does not run then, so its
          silence is the off state; only an engine that is on and silent did
          not answer. */}
      {containers.data?.enabled === false
        ? <Callout tone="neutral" title={t('services.containers.switchedOff')} />
        : containers.data?.engine.available === false
          ? <Callout tone="neutral" title={t(docker ? 'services.containers.dockerAbsent' : 'services.containers.engineAbsent', { detail: containers.data.engine.detail ?? '' })} />
          : null}
      <CollectionPanel
        title={t('services.containers.projects')}
        description={t(docker ? 'services.containers.dockerCopy' : 'services.containers.projectsCopy')}
        action={docker ? undefined : <Button size="sm" variant="outline" onClick={() => edit({ name: '', compose: TEMPLATE, creating: true })}>{t('services.containers.add')}</Button>}
      >
        {containers.isPending || projects.length === 0 ? (
          <DataTable<ProjectRow> rows={[]} rowKey={(row) => row.name} isPending={containers.isPending} empty={t(docker ? 'services.containers.dockerEmpty' : 'services.containers.empty')} emptyIcon={<Boxes />} columns={[]} />
        ) : projects.map((project) => (
          <section className="border-b last:border-b-0" key={project.name} aria-label={project.name}>
            <div className="flex flex-wrap items-center justify-between gap-2 px-4 py-3">
              <span className="flex items-center gap-2">
                <h3 className="font-mono font-medium">{project.name}</h3>
                {!docker && project.compose === undefined ? <StatusBadge tone="warning">{t('services.containers.removing')}</StatusBadge> : null}
                {project.projectDir ? <span className="font-mono text-xs text-muted-foreground" title={t('services.containers.projectDir')}>{project.projectDir}</span> : null}
              </span>
              <span className="flex gap-2">
                {verbs(projectTarget(project.name), !docker && project.compose === undefined)}
                {docker ? null : (
                  <>
                    <Button type="button" size="sm" variant="outline" disabled={project.compose === undefined} onClick={() => project.compose !== undefined && edit({ name: project.name, compose: project.compose, creating: false })}>{t('common.actions.edit')}</Button>
                    {removal(projectTarget(project.name), t('services.containers.removeProjectCopy', { name: project.name }), project.compose === undefined)}
                  </>
                )}
              </span>
            </div>
            <DataTable<ServiceRow>
              rows={project.services}
              rowKey={(row) => row.service}
              empty={t('services.containers.notTaken')}
              columns={[
                { id: 'service', header: t('services.containers.service'), cell: (row) => <span className="font-mono">{row.service}</span> },
                { id: 'image', header: t('services.containers.image'), cell: (row) => <span className="font-mono text-[0.8125rem] break-all">{row.image ?? t('common.notAvailable')}</span> },
                { id: 'state', header: t('services.containers.state'), cell: state },
                { id: 'actions', header: '', align: 'end', cell: (row) => {
                  const target = serviceTarget(project.name, row.service)
                  return (
                    <span className="flex justify-end gap-2">
                      {verbs(target, !docker && project.compose === undefined)}
                      <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.logs', { name: target.label })} onClick={() => setLogOf(target)}><FileText /></Button>
                    </span>
                  )
                } },
              ]}
            />
          </section>
        ))}
      </CollectionPanel>

      <FormDialog
        open={editing !== undefined}
        onOpenChange={(next) => { if (!next) setEditing(undefined) }}
        title={editing?.creating ? t('services.containers.add') : t('services.containers.edit', { name: editing?.name ?? '' })}
        description={t('services.containers.editCopy')}
        submitLabel={t('common.actions.save')}
        success={t('services.containers.saved', { name: editing?.name ?? '' })}
        failure={t('services.containers.title')}
        onSubmit={save}
        size="lg"
      >
        {editing?.creating ? (
          <FormField label={t('services.containers.name')} hint={t('services.containers.nameHint')}>
            {(id) => <Input id={id} className="font-mono" value={editing.name} onChange={(event) => edit({ ...editing, name: event.target.value })} required />}
          </FormField>
        ) : null}
        <FormField label={t('services.containers.compose')} hint={t('services.containers.composeHint')}>
          {(id) => <Textarea id={id} className="min-h-64 font-mono text-[0.8125rem]" value={editing?.compose ?? ''} onChange={(event) => editing && edit({ ...editing, compose: event.target.value })} spellCheck={false} required aria-invalid={checked?.ok === false || undefined} />}
        </FormField>
        <div className="flex items-center gap-2">
          <Button type="button" size="sm" variant="outline" disabled={checking || !editing?.name.trim() || !editing.compose.trim()} onClick={check}>
            {checking ? <Spinner /> : null}
            {t('services.containers.check')}
          </Button>
        </div>
        {checked?.ok ? <Callout tone="success" title={t('services.containers.checked', { count: checked.count ?? 0 })} /> : null}
        {checked && !checked.ok ? (
          <Callout tone="danger" title={t('services.containers.refused')}>
            {checked.problems?.length ? (
              <ul className="grid gap-1">
                {checked.problems.map((problem, index) => (
                  <li key={index}>
                    <span className="font-mono">{[problem.service, problem.field].filter(Boolean).join('.') || t('services.containers.wholeProject')}</span>: {problem.detail}
                  </li>
                ))}
              </ul>
            ) : checked.detail}
          </Callout>
        ) : null}
      </FormDialog>

      <Dialog open={logOf !== undefined} onOpenChange={(next) => { if (!next) setLogOf(undefined) }}>
        <DialogContent size="lg">
          <DialogHeader>
            <DialogTitle>{t('services.containers.logs', { name: logOf?.label ?? '' })}</DialogTitle>
          </DialogHeader>
          <DialogBody>
            {log.isPending ? <Spinner /> : log.error ? (
              <Callout tone="danger" title={failureDetail(log.error, t('common.requestFailed'))} />
            ) : (
              <pre className="max-h-96 overflow-auto font-mono text-xs whitespace-pre-wrap break-all">{log.data?.log || t('services.containers.logEmpty')}</pre>
            )}
          </DialogBody>
        </DialogContent>
      </Dialog>
    </div>
  )
}
