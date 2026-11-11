import type { useTranslation } from 'react-i18next'
import type { RollbackOutcome } from '@/lib/types'

type T = ReturnType<typeof useTranslation>['t']

const REASONS = ['target_failed', 'target_exhausted', 'target_missing'] as const

/// A deployment by its generation and version, or by its short ID when
/// neither is known.
function named(t: T, id: string | undefined, generation?: number | null, version?: string | null) {
  if (typeof generation !== 'number') return (id ?? '').slice(0, 12)
  const name = t('system.update.rollback.outcome.generation', { generation })
  return version ? `${name} (${version})` : name
}

/// The sentence for a rollback outcome.
export function rollbackOutcomeText(outcome: RollbackOutcome, t: T) {
  const target = named(t, outcome.target, outcome.targetGeneration, outcome.targetVersion)
  if (outcome.outcome === 'rolled_back') return t('system.update.rollback.outcome.took', { target })
  const booted = named(t, outcome.booted, outcome.bootedGeneration, outcome.bootedVersion)
  const known = REASONS.find((reason) => reason === outcome.reason)
  return known
    ? t('system.update.rollback.outcome.didNotTakeBecause', { booted, target, reason: t(`system.update.rollback.outcome.reasons.${known}`) })
    : t('system.update.rollback.outcome.didNotTake', { booted, target })
}
