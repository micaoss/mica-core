import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Undo2 } from 'lucide-react'
import { ApiError, api } from '@/shared/lib/http'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { Panel } from '@/shared/components/panel'
import { StatusDot } from '@/shared/components/status-badge'
import { failureDetail } from '@/shared/feedback/toast'

/// The parts of the update state document this pane reads. `permitted` is
/// derived from the absence of a reason, so "permitted with a reason" is not
/// a state this pane can be handed and not a state it renders. What the
/// target is comes from its own verified deployment, `null` when unknown.
interface RollbackStateDoc {
  boot?: { deploymentId?: string }
  state?: { current?: string | null, failed?: string[] }
  deployments?: { id: string, generation: number, version: string }[]
  rollback?: {
    target?: string | null
    permitted?: boolean
    reason?: string | null
    targetVersion?: string | null
    targetGeneration?: number | null
    confirmed?: boolean | null
  }
}

/// The target changed between the confirmation and the action.
class TargetChanged extends Error {}

/// `POST /api/v1/update/rollback`, 200.
interface RollbackResponse {
  deploymentId: string
  target: string
  nextStep: string
}

export function RollbackPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const read = () => api<RollbackStateDoc>('/api/v1/update')
  // The same query key the update pane reads: micad derives the eligibility
  // once, and this pane must not ask a second time and get a second answer.
  const status = useQuery({ queryKey: ['update-state'], queryFn: read })
  const decision = status.data?.rollback
  const target = decision?.target ?? undefined
  const permitted = decision?.permitted === true && !!target
  const usable = permitted && decision?.confirmed !== false
  const rollback = useMutation({
    // The target is read again first: one that changed since the operator
    // confirmed it is not acted on, and the server checks it once more.
    mutationFn: async () => {
      const fresh = await queryClient.fetchQuery({ queryKey: ['update-state'], queryFn: read, staleTime: 0 })
      if (fresh.rollback?.target !== target) throw new TargetChanged()
      return api<RollbackResponse>('/api/v1/update/rollback', { method: 'POST', body: JSON.stringify({ expectedTarget: target }) })
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['update-state'] }),
  })
  const runningId = status.data?.boot?.deploymentId
  const running = status.data?.deployments?.find((deployment) => deployment.id === runningId)
  const runningConfirmed = !!runningId && status.data?.state?.current === runningId && !status.data?.state?.failed?.includes(runningId)
  const runningText = runningId ? describe(t, runningId, running?.version, running?.generation, runningConfirmed) : undefined
  const targetText = target ? describe(t, target, decision?.targetVersion, decision?.targetGeneration, decision?.confirmed) : undefined
  const action = t('system.update.rollback.action')
  return (
    <Panel title={t('system.update.rollback.title')} description={t('system.update.rollback.description')} action={<Undo2 className="size-5 text-muted-foreground" />}>
      {runningText || targetText ? (
        <div className="grid gap-1 text-sm text-muted-foreground">
          {runningText ? <p>{t('system.update.rollback.running', { description: runningText })}</p> : null}
          {targetText ? <p>{t('system.update.rollback.retained', { description: targetText })}</p> : null}
        </div>
      ) : null}
      <div className="flex flex-wrap items-center justify-between gap-4">
        <StatusDot state={status.isPending ? 'pending' : usable ? 'ok' : 'warning'}>
          {status.isPending
            ? t('system.update.rollback.checking')
            : usable
              ? t('system.update.rollback.permitted')
              : t('system.update.rollback.refused')}
        </StatusDot>
        {status.data ? (
          <ConfirmDialog
            trigger={<Button variant="destructive" size="sm">{action}</Button>}
            disabled={!usable || rollback.isPending}
            title={action}
            description={t('system.update.rollback.confirm', {
              from: running?.version || runningId?.slice(0, 12),
              to: decision?.targetVersion || target?.slice(0, 12),
            })}
            confirmLabel={action}
            success={t('system.update.rollback.accepted')}
            failure={action}
            onConfirm={() => rollback.mutateAsync()}
          />
        ) : null}
      </div>
      {decision && !permitted ? <Callout tone="warning" title={refusalMessage(decision.reason, t)} /> : null}
      {permitted && decision?.confirmed === false ? <Callout tone="warning" title={t('system.update.rollback.reasons.targetNotConfirmed')} /> : null}
      {/* The accepted rollback stays on the page: it names the deployment that
          will boot next and is still true after a reload, unlike the toast that
          reported the click. */}
      {rollback.data ? (
        <Callout tone="success" title={t('system.update.rollback.rejected', { deploymentId: rollback.data.deploymentId, target: rollback.data.target })}>
          {t('system.update.rollback.rebootToApply')}
        </Callout>
      ) : null}
      {status.error ? <Callout tone="danger" title={failureDetail(status.error, t('common.requestFailed'))} /> : null}
      {rollback.error ? <Callout tone="danger" title={rollbackErrorMessage(rollback.error, t)} /> : null}
    </Panel>
  )
}

type T = ReturnType<typeof useTranslation>['t']

/// A deployment by its version, generation and confirmation; one without
/// version information by its short ID, never by an invented version.
function describe(t: T, id: string, version: string | null | undefined, generation: number | null | undefined, confirmed: boolean | null | undefined) {
  if (!version) return t('system.update.rollback.versionUnavailable', { id: id.slice(0, 12) })
  const parts = [version]
  if (typeof generation === 'number') parts.push(t('system.update.rollback.generation', { generation }))
  if (typeof confirmed === 'boolean') parts.push(t(confirmed ? 'system.update.rollback.confirmed' : 'system.update.rollback.notConfirmed'))
  return parts.join(' · ')
}

/// The refusal vocabulary is one set, shared by the document's `reason` and by
/// the 409's code, so one mapping answers both. An unknown verdict degrades to
/// the generic sentence rather than being reported as one of the known ones.
function refusalMessage(reason: string | null | undefined, t: T) {
  switch (reason) {
    case 'candidate_pending': return t('system.update.rollback.reasons.candidatePending')
    case 'running_not_confirmed': return t('system.update.rollback.reasons.runningNotConfirmed')
    case 'no_usable_fallback': return t('system.update.rollback.reasons.noUsableFallback')
    case 'target_changed': return t('system.update.rollback.reasons.targetChanged')
    default: return t('system.update.rollback.reasons.refused')
  }
}

function rollbackErrorMessage(error: unknown, t: T) {
  if (error instanceof TargetChanged) return t('system.update.rollback.reasons.targetChanged')
  if (error instanceof ApiError && error.status === 409) return refusalMessage(error.code, t)
  return failureDetail(error, t('common.requestFailed'))
}
