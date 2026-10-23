import { Fragment, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Check, TriangleAlert } from 'lucide-react'
import { ApiError, api, json } from '@/shared/lib/http'
import type { Health, NetworkDryRun, NetworkOverview, TaskAccepted, TaskRecord } from '@/lib/types'
import { cn } from '@/shared/lib/utils'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/shared/components/ui/dialog'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { connectionState } from '@/features/shell/connection'
import { applyStage, applyStages, stageState } from './apply-stage'
import { networkRefusal, refusedInterfaces } from './errors'
import { discardChangeSet, finishApplying, settleChangeSet, syncChangeSet, updateChangeSet } from './change-set'
import { rebase, stageRevert, touchesSession, withFreshPeers, type Change, type ChangeOp, type NetworkMap } from './model'
import { problemCount, stagedProblems } from './problems'
import { createdRows, diffRows } from './summary'
import { useKindLabel } from './parts'
import type { NetworkData } from './use-network'

const PILL: Record<ChangeOp, string> = {
  create: 'border-success/30 bg-success-background text-success',
  update: 'border-warning/40 bg-warning-background text-warning',
  delete: 'border-destructive/30 bg-danger-background text-destructive',
}

export function OpPill({ op, children }: { op: ChangeOp; children?: React.ReactNode }) {
  const { t } = useTranslation()
  return <span className={cn('inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-xs font-medium whitespace-nowrap', PILL[op])}>{t(`network.changes.ops.${op}`)}{children}</span>
}

/// The bar under every network page: what is staged, or how the last apply
/// is going. Nothing reaches the device before "Review and apply".
export function ChangeBar({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const [reviewing, setReviewing] = useState(false)
  const problems = stagedProblems(data.staged, data.pending)
  const errors = problemCount(problems)

  if (data.applying) return <Bar><ApplyStrip data={data} /></Bar>
  if (data.conflicts.length > 0) {
    return (
      <Bar>
        <TriangleAlert className="size-4 text-destructive" aria-hidden="true" />
        <strong className="text-sm">{t('network.changes.conflict', { names: data.conflicts.join(', ') })}</strong>
        <span className="flex-1" />
        <Button size="sm" variant="outline" onClick={discardChangeSet}>{t('network.changes.discardAll')}</Button>
      </Bar>
    )
  }
  if (data.pending.length === 0) return null
  return (
    <>
      <Bar label={t('network.changes.region')}>
        <strong className="text-sm">{t('network.changes.count', { count: data.pending.length })}</strong>
        <span className="flex flex-wrap gap-1.5">
          {data.pending.map((change) => <OpPill key={change.name} op={change.op}><span className="font-mono">{change.name}</span></OpPill>)}
        </span>
        {errors > 0 ? <span className="text-sm text-destructive">{t('network.changes.errors', { count: errors })}</span> : null}
        <span className="flex-1" />
        <Button size="sm" variant="outline" onClick={discardChangeSet}>{t('network.changes.discardAll')}</Button>
        <Button size="sm" disabled={errors > 0} onClick={() => setReviewing(true)}>{t('network.changes.review')}</Button>
      </Bar>
      {reviewing ? <ReviewDialog data={data} onClose={() => setReviewing(false)} /> : null}
    </>
  )
}

/// The bar while one interface is being edited: cancel puts the staged set
/// back to what it was when editing began.
export function EditBar({ what, dirty, errors, onCancel, onDone, doneLabel }: {
  what: string
  dirty: boolean
  errors: number
  onCancel: () => void
  onDone: () => void
  doneLabel?: string
}) {
  const { t } = useTranslation()
  return (
    <Bar label={t('network.edit.editing', { what })}>
      <strong className="text-sm">{t('network.edit.editing', { what })}</strong>
      <span className="text-sm text-muted-foreground">{t(dirty ? 'network.edit.dirty' : 'network.edit.clean')}</span>
      {errors > 0 ? <span className="text-sm text-destructive">{t('network.changes.errors', { count: errors })}</span> : null}
      <span className="flex-1" />
      <Button size="sm" variant="outline" onClick={onCancel}>{t('common.actions.cancel')}</Button>
      <Button size="sm" disabled={errors > 0} onClick={onDone}>{doneLabel ?? t('network.edit.done')}</Button>
    </Bar>
  )
}

function Bar({ children, label }: { children: React.ReactNode; label?: string }) {
  return (
    <div role="region" aria-label={label} className="sticky bottom-12 z-30 rounded-xl border bg-popover/95 p-3 shadow-lg backdrop-blur">
      <div className="flex flex-wrap items-center gap-3">{children}</div>
    </div>
  )
}

function useChangeRows() {
  const { t } = useTranslation()
  return (change: Change, data: NetworkData) => {
    const before = data.set?.base[change.name]
    const after = data.set?.staged[change.name]
    if (change.op === 'create' && after) return createdRows(after, change.kind, t).map(([label, value]) => [label, '', value] as const)
    if (change.op === 'delete') return []
    return diffRows(before, after, change.kind, t)
  }
}

/// Every staged change, as one diff, and the one write that applies them.
function ReviewDialog({ data, onClose }: { data: NetworkData; onClose: () => void }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const kindLabel = useKindLabel()
  const rows = useChangeRows()
  const [acknowledged, setAcknowledged] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string>()
  const [refused, setRefused] = useState<string[]>([])
  const set = data.set
  const touches = set ? touchesSession(data.pending, set, data.session) : false
  // What the device would declare on the operator's behalf: a VLAN's parent
  // nobody declared, taken over with the configuration it runs.
  const version = data.network.data?.version
  const preview = useQuery({
    queryKey: ['network-dry-run', version, set?.staged],
    queryFn: () => api<NetworkDryRun>('/api/v1/network?dryRun=true', json('PUT', { baseVersion: version, configured: set?.staged })),
    enabled: version !== undefined && set !== undefined,
    retry: false,
  })
  const adopted = preview.data?.adopted ?? []

  const apply = async () => {
    if (!set || !data.hostname) return
    setPending(true)
    setError(undefined)
    setRefused([])
    try {
      // The device's map now, not the one this set was staged on: an entry it
      // changed meanwhile is a conflict, and peers added meanwhile are kept.
      const read = await api<NetworkOverview>('/api/v1/network')
      const fresh = read.configured as NetworkMap
      const rebased = rebase(set, fresh)
      if (rebased.conflicts.length > 0) {
        syncChangeSet(data.hostname, fresh)
        onClose()
        return
      }
      const body = withFreshPeers(rebased.set.staged, fresh)
      // The read and the write are two requests: the version makes the
      // write refuse a map that moved between them.
      const accepted = await api<TaskAccepted>('/api/v1/network', json('PUT', { baseVersion: read.version, configured: body }))
      settleChangeSet(body, { taskId: accepted.taskId, previous: fresh, restoring: false })
      void queryClient.invalidateQueries({ queryKey: ['network'] })
      onClose()
    } catch (failure) {
      setPending(false)
      if (failure instanceof ApiError && failure.code === 'version_conflict') {
        const fresh = (await api<NetworkOverview>('/api/v1/network').catch(() => undefined))?.configured as NetworkMap | undefined
        if (fresh) syncChangeSet(data.hostname, fresh)
        setError(t('network.review.moved'))
        return
      }
      setError(networkRefusal(failure, t) ?? failureDetail(failure, t('common.requestFailed')))
      if (failure instanceof ApiError) {
        const named = refusedInterfaces(failure)
        const guessed = data.pending.find((change) => failure.path?.split('.').includes(change.name) || failure.message.includes(change.name))
        setRefused(named.length > 0 ? named : guessed ? [guessed.name] : [])
      }
    }
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open && !pending) onClose() }}>
      <DialogContent size="lg" showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>{t('network.review.title')}</DialogTitle>
          <DialogDescription>{t('network.review.description')}</DialogDescription>
        </DialogHeader>
        <DialogBody className="grid gap-4">
          {touches ? (
            <Callout tone="danger" title={data.session ? t('network.review.session', { name: data.session }) : t('network.review.sessionUnknown')}>
              <p>{t('network.review.sessionCopy')}</p>
              <label className="mt-2 flex items-center gap-2 font-medium text-foreground">
                <Switch checked={acknowledged} onCheckedChange={setAcknowledged} aria-label={t('network.review.acknowledge')} />
                {t('network.review.acknowledge')}
              </label>
            </Callout>
          ) : null}
          {error ? <Callout tone="danger" title={t('network.review.refused')}>{error}</Callout> : null}
          {adopted.map((name) => <Callout key={name} tone="accent" title={t('network.review.adopted', { name })} />)}
          <div className="grid gap-1 text-sm">
            {data.pending.map((change) => (
              <Fragment key={change.name}>
                <div className={cn('mt-2 flex flex-wrap items-center gap-2 border-b pb-1.5 first:mt-0', refused.includes(change.name) && 'border-destructive')}>
                  <OpPill op={change.op} />
                  <span className="font-mono font-medium">{change.name}</span>
                  <span className="text-muted-foreground">{kindLabel({ kind: change.kind, entry: set?.staged[change.name] ?? set?.base[change.name] ?? {} })}</span>
                  {refused.includes(change.name) ? <StatusBadge tone="danger">{t('network.review.refusedHere')}</StatusBadge> : null}
                  <span className="flex-1" />
                  <Button size="xs" variant="link" disabled={pending} onClick={() => updateChangeSet((current) => stageRevert(current, change.name))}>{t('network.review.revert')}</Button>
                </div>
                {change.op === 'delete' ? (
                  <div className="grid grid-cols-[minmax(0,10rem)_1fr] gap-3 py-0.5">
                    <span className="text-muted-foreground">—</span>
                    <span>{t(change.kind === 'bridge' ? 'network.review.deleteBridge' : 'network.review.delete')}</span>
                  </div>
                ) : rows(change, data).map(([label, before, after, items]) => (
                  <div key={label} className="grid grid-cols-[minmax(0,10rem)_1fr] gap-3 py-0.5">
                    <span className="text-muted-foreground">{label}</span>
                    {items ? (
                      <span className="grid font-mono text-[0.8125rem] break-words">
                        {items.added.map((item) => <ins key={`+${item}`} className="text-success no-underline">+ {item}</ins>)}
                        {items.removed.map((item) => <del key={`-${item}`} className="text-destructive no-underline">− {item}</del>)}
                      </span>
                    ) : (
                      <span className="font-mono text-[0.8125rem] break-words">
                        {change.op === 'create' ? <ins className="text-success no-underline">{after}</ins> : <><del className="text-muted-foreground">{before}</del>{'  →  '}<ins className="text-success no-underline">{after}</ins></>}
                      </span>
                    )}
                  </div>
                ))}
              </Fragment>
            ))}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button variant="outline" disabled={pending} onClick={onClose}>{t('network.review.back')}</Button>
          <Button variant={touches ? 'destructive' : 'default'} disabled={pending || data.pending.length === 0 || (touches && !acknowledged)} onClick={() => void apply()}>
            {pending ? <Spinner /> : null}
            {t('network.review.apply', { count: data.pending.length })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/// How the write is going: stored, applied by the device, reconnected. A
/// failed apply offers the map as it was before.
function ApplyStrip({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const applying = data.applying
  const health = useQuery({ queryKey: ['shell-health'], queryFn: () => api<Health>('/api/v1/health'), refetchInterval: 5_000 })
  const task = useQuery({
    queryKey: ['task', applying?.taskId],
    queryFn: () => api<TaskRecord>(`/api/v1/tasks/${encodeURIComponent(applying?.taskId ?? '')}`),
    enabled: applying !== undefined,
    refetchInterval: (query) => query.state.data?.status === 'finished' ? false : 1_000,
  })
  if (!applying) return null
  const connection = connectionState({ isError: health.isError, failureCount: health.failureCount, micad: health.data?.micad })
  const stage = applyStage(task.data, connection)
  const settled = stage === 'applied' || stage === 'failed'
  const restore = async () => {
    const read = await api<NetworkOverview>('/api/v1/network')
    const accepted = await api<TaskAccepted>('/api/v1/network', json('PUT', { baseVersion: read.version, configured: applying.previous }))
    settleChangeSet(applying.previous, { taskId: accepted.taskId, previous: applying.previous, restoring: true })
    void queryClient.invalidateQueries({ queryKey: ['network'] })
  }
  return (
    <div role="status" aria-live="polite" className="flex w-full flex-wrap items-center gap-x-5 gap-y-2">
      <strong className="text-sm">{t(applying.restoring ? 'network.apply.restoring' : 'network.apply.title')}</strong>
      <span className="flex flex-wrap gap-4 text-sm">
        {applyStages.map((step) => {
          const state = stageState(step, stage)
          return (
            <span key={step} className={state === 'done' ? 'flex items-center gap-1.5 text-success' : state === 'active' ? 'flex items-center gap-1.5 font-semibold' : 'flex items-center gap-1.5 text-muted-foreground'}>
              {state === 'done' ? <Check className="size-3.5" aria-hidden="true" /> : state === 'active' ? <Spinner className="size-3.5" /> : null}
              {t(`network.apply.steps.${step}`)}
            </span>
          )
        })}
      </span>
      <span className="text-sm text-muted-foreground">{stage === 'failed' && task.data?.message ? task.data.message : t(`network.apply.body.${stage}`)}</span>
      <span className="flex-1" />
      {stage === 'failed' && !applying.restoring ? (
        <ConfirmDialog
          trigger={<Button size="sm" variant="destructive">{t('network.apply.restore')}</Button>}
          title={t('network.apply.restore')}
          description={t('network.apply.restoreCopy')}
          confirmLabel={t('network.apply.restore')}
          success={t('network.apply.restoreSent')}
          failure={t('network.apply.restore')}
          onConfirm={restore}
        />
      ) : null}
      {settled ? <Button size="sm" variant="outline" onClick={finishApplying}>{t('network.apply.dismiss')}</Button> : null}
    </div>
  )
}
