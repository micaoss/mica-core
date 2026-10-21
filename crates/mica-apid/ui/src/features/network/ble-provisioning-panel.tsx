import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { BleProvisioning } from '@/lib/types'
import { api, json } from '@/shared/lib/http'
import { Callout } from '@/shared/components/callout'
import { DataTable } from '@/shared/components/data-table'
import { formatDeviceClock } from '@/shared/components/fact'
import { FactList } from '@/shared/components/fact-list'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { StatusBadge, type StatusTone } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { PROVISIONING_PAGE, groupCode, provisioningQuery, remaining, useClock } from './bluetooth'

type Minutes = '5' | '10' | '30'
type Entry = BleProvisioning['history'][number]

const TONE: Record<BleProvisioning['state'], StatusTone> = {
  off: 'neutral',
  waiting: 'warning',
  connected: 'accent',
  done: 'success',
}

/// BLE provisioning: its state, the console's manual window with its
/// temporary code, and the last ten sessions.
///
/// A window opened by boot or by losing the network closes by itself; the
/// console closes only the manual one, and opening a manual window over
/// another makes the temporary code the way in.
export function BleProvisioningPanel() {
  const { t, i18n: activeI18n } = useTranslation()
  const queryClient = useQueryClient()
  // A phone that connects shows up without a refresh.
  const status = useQuery({
    ...provisioningQuery,
    refetchInterval: (query) => query.state.data && query.state.data.state !== 'off' ? 2_000 : 15_000,
  })
  const value = status.data
  const [minutes, setMinutes] = useState<Minutes>('10')
  const now = useClock(value?.expiresAt != null)
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: provisioningQuery.queryKey })

  const openWindow = useMutationFeedback<BleProvisioning, number>({
    mutationFn: (seconds) => api<BleProvisioning>('/api/v1/bluetooth/provisioning', json('POST', { durationSeconds: seconds })),
    success: t('network.bluetooth.provisioning.opened'),
    failure: t('network.bluetooth.provisioning.open'),
    onSuccess: (data) => queryClient.setQueryData(provisioningQuery.queryKey, data),
  })
  const closeWindow = useMutationFeedback<void, void>({
    mutationFn: () => api<void>('/api/v1/bluetooth/provisioning', { method: 'DELETE' }),
    success: t('network.bluetooth.provisioning.closed'),
    failure: t('network.bluetooth.provisioning.close'),
    onSuccess: invalidate,
  })

  // The device closes the window; once its time is up the page asks again.
  const expired = value?.expiresAt != null && value.expiresAt * 1000 <= now
  useEffect(() => {
    if (expired) invalidate()
  }, [expired])

  const clock = (seconds: number) => t('common.deviceClock', { time: formatDeviceClock(seconds, activeI18n.resolvedLanguage ?? 'en') })
  const open = value?.trigger != null
  const locked = value?.lockedUntil != null
  return (
    <>
      <Panel title={t('network.bluetooth.provisioning.title')} description={t('network.bluetooth.provisioning.copy')}>
        {status.error ? <Callout tone="warning" title={failureDetail(status.error, t('network.bluetooth.provisioning.unavailable'))} /> : null}
        {value && !value.verifierReady ? <Callout tone="warning" title={t('network.bluetooth.provisioning.verifierMissing')} /> : null}
        {value?.lockedUntil != null ? <Callout tone="danger" title={t('network.bluetooth.provisioning.locked', { until: clock(value.lockedUntil) })} /> : null}
        {value ? (
          <FactList facts={[
            { id: 'state', label: t('network.bluetooth.provisioning.state'), value: <StatusBadge tone={TONE[value.state]}>{t(`network.bluetooth.provisioning.states.${value.state}`)}</StatusBadge> },
            value.trigger ? { id: 'trigger', label: t('network.bluetooth.provisioning.trigger'), value: t(`network.bluetooth.provisioning.triggers.${value.trigger}`) } : undefined,
            open ? { id: 'left', label: t('network.bluetooth.provisioning.left'), value: <span data-testid="provisioning-left">{value.expiresAt != null ? remaining(value.expiresAt, now) : t('network.bluetooth.provisioning.untilRoute')}</span> } : undefined,
            { id: 'page', label: t('network.bluetooth.provisioning.page'), value: (
              <span className="grid">
                <span className="font-mono">{PROVISIONING_PAGE}</span>
                <span className="text-sm text-muted-foreground">{t('network.bluetooth.provisioning.pageCopy')}</span>
              </span>
            ) },
          ]} />
        ) : null}
        {value?.code ? (
          <div className="grid gap-1 rounded-md border p-4">
            <span className="text-sm text-muted-foreground">{t('network.bluetooth.provisioning.code')}</span>
            <span className="font-mono text-4xl tracking-widest" data-testid="provisioning-code">{groupCode(value.code)}</span>
            <span className="text-sm text-muted-foreground">{t('network.bluetooth.provisioning.codeCopy')}</span>
          </div>
        ) : null}
        {value?.trigger === 'manual' ? (
          <div>
            <Button variant="destructive" disabled={closeWindow.isPending} onClick={() => closeWindow.mutate()}>{t('network.bluetooth.provisioning.close')}</Button>
          </div>
        ) : value ? (
          <div className="flex flex-wrap items-center gap-3">
            <SegmentedControl<Minutes>
              label={t('network.bluetooth.provisioning.duration')}
              value={minutes}
              onValueChange={setMinutes}
              segments={(['5', '10', '30'] as const).map((each) => ({ value: each, label: t('network.bluetooth.provisioning.minutes', { count: Number(each) }) }))}
            />
            <Button disabled={openWindow.isPending || locked} onClick={() => openWindow.mutate(Number(minutes) * 60)}>{t('network.bluetooth.provisioning.open')}</Button>
            <span className="text-sm text-muted-foreground">{t('network.bluetooth.provisioning.oneAtATime')}</span>
          </div>
        ) : null}
      </Panel>

      <CollectionPanel title={t('network.bluetooth.provisioning.history')}>
        <DataTable<Entry>
          rows={value?.history ?? []}
          rowKey={(entry) => `${entry.at}-${entry.outcome}`}
          isPending={status.isPending}
          empty={t('network.bluetooth.provisioning.historyEmpty')}
          columns={[
            { id: 'at', header: t('network.bluetooth.provisioning.at'), cell: (entry) => clock(entry.at) },
            { id: 'trigger', header: t('network.bluetooth.provisioning.trigger'), cell: (entry) => t(`network.bluetooth.provisioning.triggers.${entry.trigger}`) },
            { id: 'outcome', header: t('network.bluetooth.provisioning.outcome'), cell: (entry) => t(`network.bluetooth.provisioning.outcomes.${entry.outcome}`, { defaultValue: entry.outcome }) },
          ]}
        />
      </CollectionPanel>
    </>
  )
}
