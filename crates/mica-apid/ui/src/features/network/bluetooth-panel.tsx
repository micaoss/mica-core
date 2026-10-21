import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Bluetooth, Plus, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { BluetoothOverview, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FactList } from '@/shared/components/fact-list'
import { ToggleField } from '@/shared/components/form-field'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { bluetoothRows, remaining, type BluetoothRow } from './bluetooth'

type Pending = BluetoothOverview['pending'][number]

/// The adapter, the devices it is paired with, and the pairing window.
///
/// The adapter is never discoverable by a setting: "Allow new devices" opens
/// it for three minutes, and a phone that pairs inside them shows up under
/// "Pairing requests" with the six digits both ends display. The window closes
/// on the device, whether or not this page is open.
export function BluetoothPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  // While the window is open the page polls faster: a request has to appear
  // without a refresh, and the phone waits for it only about half a minute.
  const bluetooth = useQuery({
    queryKey: ['bluetooth'],
    queryFn: () => api<BluetoothOverview>('/api/v1/bluetooth'),
    refetchInterval: (query) => query.state.data?.pairWindow ? 2_000 : 15_000,
  })
  const overview = bluetooth.data
  const window = overview?.pairWindow ?? null
  const now = useClock(window !== null)
  const rows = bluetoothRows(overview)
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['bluetooth'] })

  const power = useMutationFeedback<TaskAccepted, boolean>({
    mutationFn: (powered) => api<TaskAccepted>('/api/v1/bluetooth', json('PUT', { powered })),
    success: t('network.bluetooth.saved'),
    failure: t('network.bluetooth.save'),
    onSuccess: invalidate,
  })
  const openWindow = useMutationFeedback<{ expiresAt: number }, void>({
    mutationFn: () => api<{ expiresAt: number }>('/api/v1/bluetooth/pairing-window', { method: 'POST' }),
    success: t('network.bluetooth.windowOpened'),
    failure: t('network.bluetooth.allowPairing'),
    onSuccess: invalidate,
  })
  const closeWindow = useMutationFeedback<void, void>({
    mutationFn: () => api<void>('/api/v1/bluetooth/pairing-window', { method: 'DELETE' }),
    success: t('network.bluetooth.windowClosed'),
    failure: t('network.bluetooth.stop'),
    onSuccess: invalidate,
  })
  const answer = useMutationFeedback<void, { request: Pending; accept: boolean }>({
    mutationFn: ({ request, accept }) => api<void>(`/api/v1/bluetooth/pairing-requests/${encodeURIComponent(request.mac)}`, json('POST', { accept })),
    success: (_data, { request, accept }) => t(accept ? 'network.bluetooth.confirmed' : 'network.bluetooth.rejected', { name: request.name || request.mac }),
    failure: t('network.bluetooth.answer'),
    onSuccess: invalidate,
  })
  const remove = useMutationFeedback<void, string>({
    mutationFn: (address) => api<void>(`/api/v1/bluetooth/devices/${encodeURIComponent(address)}`, { method: 'DELETE' }),
    success: (_data, address) => t('network.bluetooth.removed', { address }),
    failure: t('network.bluetooth.remove'),
    onSuccess: invalidate,
  })

  // The device closes the window; once its time is up the page asks again
  // rather than counting below zero.
  const expired = window !== null && window.expiresAt * 1000 <= now
  useEffect(() => {
    if (expired) invalidate()
  }, [expired])

  const left = window ? remaining(window.expiresAt, now) : ''
  return (
    <div className="grid gap-3">
      {bluetooth.error ? <Callout tone="danger" title={failureDetail(bluetooth.error, t('common.requestFailed'))} /> : null}
      {overview?.adapter.available === false ? <Callout tone="warning" title={overview.adapter.detail ?? t('network.bluetooth.noAdapter')} /> : null}

      <Panel title={t('network.bluetooth.title')} description={t('network.bluetooth.copy')} action={<Bluetooth className="size-5 text-muted-foreground" />}>
        <ToggleField
          title={t('network.bluetooth.enabled')}
          description={t('network.bluetooth.enabledCopy')}
          control={<Switch checked={overview?.enabled ?? false} disabled={!overview || power.isPending} onCheckedChange={(value) => power.mutate(value)} aria-label={t('network.bluetooth.enabled')} />}
        />
      </Panel>

      {overview?.adapter.available ? (
        <Panel title={t('network.bluetooth.adapter')} description={t('network.bluetooth.adapterCopy')}>
          <FactList facts={[
            { id: 'name', label: t('network.bluetooth.name'), value: <span className="font-mono">{overview.adapter.name || '—'}</span> },
            { id: 'address', label: t('network.bluetooth.address'), value: <span className="font-mono">{overview.adapter.address ?? '—'}</span> },
            { id: 'powered', label: t('network.bluetooth.state'), value: overview.adapter.powered ? t('network.bluetooth.on') : t('network.bluetooth.off') },
            { id: 'discoverable', label: t('network.bluetooth.discoverable'), value: window ? <span className="text-warning">{t('network.bluetooth.discoverableFor', { left })}</span> : t('network.bluetooth.notDiscoverable') },
          ]} />
        </Panel>
      ) : null}

      <CollectionPanel
        title={t('network.bluetooth.devices')}
        description={window ? undefined : t('network.bluetooth.windowCopy')}
        action={window ? (
          <span className="flex items-center gap-2">
            <span className="text-sm text-warning" data-testid="pair-window">{t('network.bluetooth.allowing', { left })}</span>
            <Button size="sm" variant="outline" disabled={closeWindow.isPending} onClick={() => closeWindow.mutate()}>{t('network.bluetooth.stop')}</Button>
          </span>
        ) : (
          <Button size="sm" variant="outline" disabled={openWindow.isPending || !overview?.enabled || overview.adapter.available === false} onClick={() => openWindow.mutate()}>
            <Plus />{t('network.bluetooth.allowPairing')}
          </Button>
        )}
      >
        <DataTable<BluetoothRow>
          rows={rows}
          rowKey={(row) => row.address}
          isPending={bluetooth.isPending}
          empty={t('network.bluetooth.empty')}
          columns={[
            { id: 'device', header: t('network.bluetooth.device'), cell: (row) => (
              <span className="grid">
                <span>{row.name || t('network.bluetooth.unnamed')}</span>
                <span className="font-mono text-[0.8125rem] text-muted-foreground">{row.address}</span>
              </span>
            ) },
            { id: 'state', header: t('network.bluetooth.state'), cell: (row) => (
              <span className="flex flex-wrap gap-1">
                {row.connected ? <StatusBadge tone="success">{t('network.bluetooth.connected')}</StatusBadge> : null}
                {row.trusted ? <StatusBadge tone="neutral">{t('network.bluetooth.trusted')}</StatusBadge> : null}
                {row.blocked ? <StatusBadge tone="danger">{t('network.bluetooth.blocked')}</StatusBadge> : null}
                {row.seen ? null : <StatusBadge tone="neutral">{t('network.bluetooth.outOfRange')}</StatusBadge>}
              </span>
            ) },
            { id: 'actions', header: '', align: 'end', cell: (row) => (
              <ConfirmDialog
                trigger={<Button type="button" size="icon-sm" variant="destructive" aria-label={t('network.bluetooth.remove', { address: row.address })}><Trash2 /></Button>}
                title={t('network.bluetooth.remove', { address: row.address })}
                description={t('network.bluetooth.removeCopy')}
                confirmLabel={t('network.bluetooth.remove', { address: row.address })}
                success={t('network.bluetooth.removed', { address: row.address })}
                failure={t('network.bluetooth.remove', { address: row.address })}
                onConfirm={() => remove.mutateAsync(row.address)}
              />
            ) },
          ]}
        />
      </CollectionPanel>

      <CollectionPanel title={t('network.bluetooth.pendingTitle')} description={t('network.bluetooth.pendingCopy')}>
        <DataTable<Pending>
          rows={overview?.pending ?? []}
          rowKey={(request) => request.mac}
          isPending={bluetooth.isPending}
          empty={window ? t('network.bluetooth.waiting') : t('network.bluetooth.noRequests')}
          columns={[
            { id: 'device', header: t('network.bluetooth.device'), cell: (request) => (
              <span className="grid">
                <span>{request.name || t('network.bluetooth.unnamed')}</span>
                <span className="font-mono text-[0.8125rem] text-muted-foreground">{request.mac}</span>
              </span>
            ) },
            { id: 'passkey', header: t('network.bluetooth.passkey'), cell: (request) => (
              <span className="font-mono text-2xl tracking-widest">{request.passkey ?? t('network.bluetooth.noPasskey')}</span>
            ) },
            { id: 'actions', header: '', align: 'end', cell: (request) => (
              <span className="flex justify-end gap-2">
                <Button size="sm" disabled={answer.isPending} onClick={() => answer.mutate({ request, accept: true })}>{t('network.bluetooth.accept')}</Button>
                <Button size="sm" variant="outline" disabled={answer.isPending} onClick={() => answer.mutate({ request, accept: false })}>{t('network.bluetooth.reject')}</Button>
              </span>
            ) },
          ]}
        />
      </CollectionPanel>
    </div>
  )
}

/// The time now, ticking every second while `running`.
function useClock(running: boolean) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    setNow(Date.now())
    const timer = globalThis.setInterval(() => setNow(Date.now()), 1_000)
    return () => globalThis.clearInterval(timer)
  }, [running])
  return now
}
