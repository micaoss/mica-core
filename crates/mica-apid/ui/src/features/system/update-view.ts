/// The update summary as two separate answers, derived from the lifecycle
/// fields `GET /api/v1/update` already carries: whether an update is on offer,
/// and what the device is doing about one. A failed check is an availability
/// answer; it is never a failed install.

export type Availability = 'not_checked' | 'checking' | 'up_to_date' | 'available' | 'check_failed'
export type Operation = 'idle' | 'downloading' | 'installing' | 'restart_required' | 'trial_boot' | 'failed'

export interface UpdateViewDoc {
  lifecycle?: {
    state?: string
    failedDuring?: string
    available?: unknown
    last_check?: string
  }
  install?: { status?: string }
  boot?: { deploymentId?: string }
  state?: { current?: string | null; candidate?: string | null }
  sets?: { core?: { pending?: { id: string } | null; boot?: { coreSetId?: string } } }
}

export function updateView(doc: UpdateViewDoc): { availability: Availability; operation: Operation } {
  const lifecycle = doc.lifecycle
  const state = lifecycle?.state
  const checkFailed = state === 'failed' && lifecycle?.failedDuring === 'check'
  const availability: Availability = state === 'checking' ? 'checking'
    : checkFailed ? 'check_failed'
    : lifecycle?.available ? 'available'
    : lifecycle?.last_check ? 'up_to_date'
    : 'not_checked'

  const running = doc.boot?.deploymentId
  const candidate = doc.state?.candidate
  const core = doc.sets?.core
  const operation: Operation = state === 'downloading' ? 'downloading'
    : state === 'installing' ? 'installing'
    : (state === 'failed' && !checkFailed) || doc.install?.status === 'failed' ? 'failed'
    : (candidate && candidate !== running) || (core?.pending && core.boot?.coreSetId !== core.pending.id) ? 'restart_required'
    : (candidate && candidate === running) || (running && doc.state?.current && doc.state.current !== running) || core?.pending ? 'trial_boot'
    : 'idle'
  return { availability, operation }
}
