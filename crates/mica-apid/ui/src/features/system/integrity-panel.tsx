import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { RefreshCw, ShieldCheck } from 'lucide-react'
import { api } from '@/shared/lib/http'
import type { IntegrityItem, SystemIntegrity } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { Panel } from '@/shared/components/panel'
import { StatusBadge, type StatusTone } from '@/shared/components/status-badge'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'

const TONES: Record<IntegrityItem['state'], StatusTone> = {
  ok: 'success',
  attention: 'warning',
  unknown: 'neutral',
  not_supported: 'neutral',
}

/// A device time as `YYYY-MM-DD HH:MM UTC`.
function utc(at: string) {
  return `${at.slice(0, 10)} ${at.slice(11, 16)} UTC`
}

/// The integrity card: each check the device reports, its state and what it
/// rests on. It is read on entry and on Refresh; nothing is verified again.
export function IntegrityPanel() {
  const { t } = useTranslation()
  const integrity = useQuery({
    queryKey: ['system-integrity'],
    queryFn: () => api<SystemIntegrity>('/api/v1/system/integrity'),
    retry: false,
  })
  const value = integrity.data
  const refresh = (
    <Button variant="outline" size="sm" onClick={() => void integrity.refetch()} disabled={integrity.isFetching}>
      {integrity.isFetching ? <Spinner /> : <RefreshCw />}
      {t('system.information.integrity.refresh')}
    </Button>
  )
  return (
    <Panel title={t('system.information.integrity.title')} description={t('system.information.integrity.description')} action={value ? refresh : <ShieldCheck className="size-5 text-muted-foreground" />} contentClassName="gap-3">
      {integrity.isPending ? <p className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />{t('system.information.integrity.loading')}</p> : null}
      {integrity.error ? <Callout tone="danger" title={failureDetail(integrity.error, t('common.requestFailed'))} /> : null}
      {value ? (
        <>
          <ul className="grid gap-2">
            {value.items.map((item) => (
              <li key={item.key} className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 border-b pb-2 last:border-b-0 last:pb-0">
                <span className="text-sm font-medium">{t(`system.information.integrity.items.${item.key}`)}</span>
                <span className="flex flex-wrap items-baseline gap-2 text-sm text-muted-foreground">
                  {detail(item, t)}
                  <StatusBadge tone={TONES[item.state]}>{t(`system.information.integrity.states.${item.state}`)}</StatusBadge>
                </span>
              </li>
            ))}
          </ul>
          <p className="text-xs text-muted-foreground">{t('system.information.integrity.checkedAt', { at: utc(value.checkedAt) })}</p>
          {value.extensions.length ? (
            <div className="grid gap-1">
              <p className="text-sm font-medium">{t('system.information.integrity.extensionsTitle')}</p>
              {value.extensions.map((extension) => (
                <p key={extension.key} className="text-sm text-muted-foreground">{extensionText(extension, t)}</p>
              ))}
            </div>
          ) : null}
        </>
      ) : null}
    </Panel>
  )
}

type T = ReturnType<typeof useTranslation>['t']

function detail(item: IntegrityItem, t: T) {
  const parts: string[] = []
  if (item.detailCode) {
    parts.push(t(`system.information.integrity.details.${item.detailCode}`, {
      version: item.value ?? '',
      domains: (item.domains ?? []).join(', '),
    }))
  }
  if (item.key === 'deploymentConfirmed' && typeof item.value === 'number') {
    parts.push(t('system.information.integrity.generation', { generation: item.value }))
  }
  if (item.checkedAt) parts.push(t('system.information.integrity.lastCheck', { at: utc(item.checkedAt) }))
  return parts.length ? <span>{parts.join(' · ')}</span> : null
}

function extensionText(extension: SystemIntegrity['extensions'][number], t: T) {
  if (extension.key === 'sshAuthorizedKeys') return t('system.information.integrity.extensions.sshAuthorizedKeys', { count: Number(extension.value) })
  return t(extension.value ? 'system.information.integrity.extensions.customUiOn' : 'system.information.integrity.extensions.customUiOff')
}
