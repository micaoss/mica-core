import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Boxes, Play, RotateCw, Square, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { ContainerOverview, ContainerUnit, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { CollectionPanel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { containerRows, parseList, parsePorts, parseVolumes, formatPorts, formatVolumes, formatEnvironment, parseEnvironment, type ContainerRow } from './containers'

const RESTART_POLICIES = ['no', 'on-failure', 'always'] as const

/// The containers this device declares, with what the engine says about each.
///
/// Declared and observed are shown in one row but read from two places: the
/// settings map says what should run, `podman ps` says what does. A container
/// declared and never started, and one running that nobody declared, are both
/// real states and the row says which it is.
export function ContainersPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<{ name: string; draft: Draft; creating: boolean }>()
  const containers = useQuery({ queryKey: ['containers'], queryFn: () => api<ContainerOverview>('/api/v1/containers'), refetchInterval: 10_000 })
  const rows = containerRows(containers.data)

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['containers'] })
  }
  const act = useMutationFeedback<void, { name: string; action: 'start' | 'stop' | 'restart' }>({
    mutationFn: ({ name, action }) => api<void>(`/api/v1/containers/${encodeURIComponent(name)}/${action}`, { method: 'POST' }),
    success: (_data, { name, action }) => t(`services.containers.acted.${action}`, { name }),
    failure: t('services.containers.title'),
    onSuccess: invalidate,
  })
  const remove = useMutationFeedback<TaskAccepted, string>({
    mutationFn: (name) => api<TaskAccepted>(`/api/v1/containers/${encodeURIComponent(name)}`, { method: 'DELETE' }),
    success: (_data, name) => t('services.containers.removed', { name }),
    failure: t('services.containers.remove'),
    onSuccess: invalidate,
  })

  const save = () => {
    if (!editing) return Promise.resolve()
    const { name, draft } = editing
    const unit: ContainerUnit = {
      image: draft.image.trim(),
      command: parseList(draft.command),
      environment: parseEnvironment(draft.environment),
      publish: parsePorts(draft.publish),
      volumes: parseVolumes(draft.volumes),
      restart: draft.restart,
      autoStart: draft.autoStart,
    }
    return api<TaskAccepted>(`/api/v1/containers/${encodeURIComponent(name)}`, json('PUT', unit)).then(() => {
      setEditing(undefined)
      invalidate()
    })
  }

  return (
    <div className="grid gap-3">
      {containers.error ? <Callout tone="danger" title={failureDetail(containers.error, t('common.requestFailed'))} /> : null}
      {containers.data && containers.data.enabled === false ? <Callout tone="warning" title={t('services.containers.switchedOff')} /> : null}
      {containers.data?.engine.available === false ? <Callout tone="neutral" title={t('services.containers.engineAbsent', { detail: containers.data.engine.detail ?? '' })} /> : null}
      <CollectionPanel
        title={t('services.containers.declared')}
        action={<Button size="sm" variant="outline" onClick={() => setEditing({ name: '', draft: emptyDraft(), creating: true })}>{t('services.containers.add')}</Button>}
      >
        <DataTable<ContainerRow>
          rows={rows}
          rowKey={(row) => row.name}
          isPending={containers.isPending}
          empty={t('services.containers.empty')}
          emptyIcon={<Boxes />}
          columns={[
            { id: 'name', header: t('services.containers.name'), cell: (row) => <span className="font-mono">{row.name}</span> },
            { id: 'image', header: t('services.containers.image'), cell: (row) => <span className="font-mono text-[0.8125rem] break-all">{row.declared?.image ?? t('common.notAvailable')}</span> },
            { id: 'state', header: t('services.containers.state'), cell: (row) => (
              <StatusBadge tone={row.state === 'running' ? 'success' : row.state === undefined ? 'neutral' : 'warning'}>
                {row.state ?? t('services.containers.notObserved')}
              </StatusBadge>
            ) },
            { id: 'ports', header: t('services.containers.ports'), cell: (row) => <span className="font-mono text-[0.8125rem]">{formatPorts(row.declared?.publish ?? []) || '—'}</span> },
            { id: 'actions', header: '', align: 'end', cell: (row) => (
              <span className="flex justify-end gap-2">
                <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.start', { name: row.name })} disabled={!row.declared} onClick={() => act.mutate({ name: row.name, action: 'start' })}><Play /></Button>
                <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.restart', { name: row.name })} disabled={!row.declared} onClick={() => act.mutate({ name: row.name, action: 'restart' })}><RotateCw /></Button>
                <Button type="button" size="icon-sm" variant="outline" aria-label={t('services.containers.stop', { name: row.name })} disabled={!row.declared} onClick={() => act.mutate({ name: row.name, action: 'stop' })}><Square /></Button>
                <Button type="button" size="sm" variant="outline" disabled={!row.declared} onClick={() => row.declared && setEditing({ name: row.name, draft: draftOf(row.declared), creating: false })}>{t('common.actions.edit')}</Button>
                <ConfirmDialog
                  trigger={<Button type="button" size="icon-sm" variant="destructive" aria-label={t('services.containers.remove', { name: row.name })} disabled={!row.declared}><Trash2 /></Button>}
                  title={t('services.containers.remove', { name: row.name })}
                  description={t('services.containers.removeCopy', { name: row.name })}
                  confirmLabel={t('services.containers.remove', { name: row.name })}
                  success={t('services.containers.removed', { name: row.name })}
                  failure={t('services.containers.remove', { name: row.name })}
                  onConfirm={() => remove.mutateAsync(row.name)}
                />
              </span>
            ) },
          ]}
        />
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
      >
        {editing?.creating ? (
          <FormField label={t('services.containers.name')} hint={t('services.containers.nameHint')}>
            {(id) => <Input id={id} className="font-mono" value={editing.name} onChange={(event) => setEditing({ ...editing, name: event.target.value })} required />}
          </FormField>
        ) : null}
        <FormField label={t('services.containers.image')}>
          {(id) => <Input id={id} className="font-mono" value={editing?.draft.image ?? ''} placeholder="docker.io/library/busybox:1" onChange={(event) => editing && setEditing({ ...editing, draft: { ...editing.draft, image: event.target.value } })} required />}
        </FormField>
        <FormField label={t('services.containers.command')} hint={t('services.containers.commandHint')}>
          {(id) => <Input id={id} className="font-mono" value={editing?.draft.command ?? ''} onChange={(event) => editing && setEditing({ ...editing, draft: { ...editing.draft, command: event.target.value } })} />}
        </FormField>
        <FormField label={t('services.containers.env')} hint={t('services.containers.envHint')}>
          {(id) => <Input id={id} className="font-mono" value={editing?.draft.environment ?? ''} placeholder="TZ=UTC, LOG=debug" onChange={(event) => editing && setEditing({ ...editing, draft: { ...editing.draft, environment: event.target.value } })} />}
        </FormField>
        <FormField label={t('services.containers.ports')} hint={t('services.containers.portsHint')}>
          {(id) => <Input id={id} className="font-mono" value={editing?.draft.publish ?? ''} placeholder="8080:80, 5353:53/udp" onChange={(event) => editing && setEditing({ ...editing, draft: { ...editing.draft, publish: event.target.value } })} />}
        </FormField>
        <FormField label={t('services.containers.volumes')} hint={t('services.containers.volumesHint')}>
          {(id) => <Input id={id} className="font-mono" value={editing?.draft.volumes ?? ''} placeholder="/mica/apps/app:/data" onChange={(event) => editing && setEditing({ ...editing, draft: { ...editing.draft, volumes: event.target.value } })} />}
        </FormField>
        <FormField label={t('services.containers.restartPolicy')}>
          {(id) => (
            <Select value={editing?.draft.restart ?? 'no'} onValueChange={(value) => editing && setEditing({ ...editing, draft: { ...editing.draft, restart: String(value) as Draft['restart'] } })}>
              <SelectTrigger id={id} aria-label={t('services.containers.restartPolicy')}><SelectValue /></SelectTrigger>
              <SelectContent>{RESTART_POLICIES.map((option) => <SelectItem value={option} key={option}>{t(`services.containers.restarts.${option}`)}</SelectItem>)}</SelectContent>
            </Select>
          )}
        </FormField>
        <ToggleField
          title={t('services.containers.autoStart')}
          description={t('services.containers.autoStartCopy')}
          control={<Switch checked={editing?.draft.autoStart ?? false} onCheckedChange={(value) => editing && setEditing({ ...editing, draft: { ...editing.draft, autoStart: value } })} aria-label={t('services.containers.autoStart')} />}
        />
      </FormDialog>
    </div>
  )
}

/// The form's own shape: the lists are text while they are being typed, and
/// become the device's shape on save.
interface Draft {
  image: string
  command: string
  environment: string
  publish: string
  volumes: string
  restart: (typeof RESTART_POLICIES)[number]
  autoStart: boolean
}

function emptyDraft(): Draft {
  return { image: '', command: '', environment: '', publish: '', volumes: '', restart: 'no', autoStart: true }
}

function draftOf(unit: ContainerUnit): Draft {
  return {
    image: unit.image,
    command: (unit.command ?? []).join(' '),
    environment: formatEnvironment(unit.environment ?? {}),
    publish: formatPorts(unit.publish ?? []),
    volumes: formatVolumes(unit.volumes ?? []),
    restart: (unit.restart ?? 'no') as Draft['restart'],
    autoStart: unit.autoStart ?? false,
  }
}
