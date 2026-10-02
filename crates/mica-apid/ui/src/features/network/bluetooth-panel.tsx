import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Bluetooth, Radar, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { BluetoothOverview, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { bluetoothRows, type BluetoothRow } from './bluetooth'

/// The adapter, what it can see, and what it trusts.
///
/// Declared and observed are two lists joined by address and shown in one
/// table that says which side each row came from: a paired device out of range
/// is declared and not seen, and a device in range that nobody paired is seen
/// and not declared.
export function BluetoothPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  // While a pairing is waiting for a decision the page polls faster: the
  // dialog is the one thing here that has to appear without a refresh.
  const bluetooth = useQuery({
    queryKey: ['bluetooth'],
    queryFn: () => api<BluetoothOverview>('/api/v1/bluetooth'),
    refetchInterval: (query) => query.state.data?.adapter.discovering || query.state.data?.pending ? 2_000 : 15_000,
  })
  const [draft, setDraft] = useState<{ alias?: string; pin?: string }>({})

  const overview = bluetooth.data
  const rows = bluetoothRows(overview)
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['bluetooth'] })

  const save = useMutationFeedback<TaskAccepted, { enabled: boolean; discoverable: boolean; alias?: string; pin?: string }>({
    mutationFn: (body) => api<TaskAccepted>('/api/v1/bluetooth', json('PUT', body)),
    success: t('network.bluetooth.saved'),
    failure: t('network.bluetooth.save'),
    onSuccess: invalidate,
  })
  const discovery = useMutationFeedback<void, boolean>({
    mutationFn: (on) => api<void>('/api/v1/bluetooth/discovery', json('POST', { on })),
    success: (_data, on) => t(on ? 'network.bluetooth.scanning' : 'network.bluetooth.scanStopped'),
    failure: t('network.bluetooth.scan'),
    onSuccess: invalidate,
  })
  const pair = useMutationFeedback<void, string>({
    mutationFn: (address) => api<void>(`/api/v1/bluetooth/devices/${encodeURIComponent(address)}/pair`, { method: 'POST' }),
    success: (_data, address) => t('network.bluetooth.paired', { address }),
    failure: t('network.bluetooth.pair'),
    onSuccess: invalidate,
  })
  const confirm = useMutationFeedback<void, { address: string; accept: boolean }>({
    mutationFn: ({ address, accept }) => api<void>(`/api/v1/bluetooth/devices/${encodeURIComponent(address)}/confirm`, json('POST', { accept })),
    success: (_data, { accept }) => t(accept ? 'network.bluetooth.confirmed' : 'network.bluetooth.rejected'),
    failure: t('network.bluetooth.confirm'),
    onSuccess: invalidate,
  })
  const remove = useMutationFeedback<void, string>({
    mutationFn: (address) => api<void>(`/api/v1/bluetooth/devices/${encodeURIComponent(address)}`, { method: 'DELETE' }),
    success: (_data, address) => t('network.bluetooth.removed', { address }),
    failure: t('network.bluetooth.remove'),
    onSuccess: invalidate,
  })

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!overview) return
    save.mutate({
      enabled: overview.enabled,
      discoverable: overview.discoverable,
      // Empty means "derive it", which is what an absent key says to the
      // device; sending an empty string would store one.
      ...(draft.alias?.trim() ? { alias: draft.alias.trim() } : {}),
      ...(draft.pin?.trim() ? { pin: draft.pin.trim() } : {}),
    })
  }
  const toggle = (change: { enabled?: boolean; discoverable?: boolean }) => {
    if (!overview) return
    save.mutate({ enabled: overview.enabled, discoverable: overview.discoverable, ...change })
  }

  return (
    <div className="grid gap-3">
      {bluetooth.error ? <Callout tone="danger" title={failureDetail(bluetooth.error, t('common.requestFailed'))} /> : null}
      {overview?.adapter.available === false ? <Callout tone="warning" title={overview.adapter.detail ?? t('network.bluetooth.noAdapter')} /> : null}

      {overview?.pending ? (
        <Panel className="border-primary" title={t('network.bluetooth.pendingTitle')} description={t('network.bluetooth.pendingCopy', { address: overview.pending.address })}>
          <p className="font-mono text-3xl tracking-widest">{String(overview.pending.passkey).padStart(6, '0')}</p>
          <div className="flex flex-wrap gap-2">
            <Button onClick={() => confirm.mutate({ address: overview.pending!.address, accept: true })} disabled={confirm.isPending}>
              {t('network.bluetooth.accept')}
            </Button>
            <Button variant="outline" onClick={() => confirm.mutate({ address: overview.pending!.address, accept: false })} disabled={confirm.isPending}>
              {t('network.bluetooth.reject')}
            </Button>
          </div>
        </Panel>
      ) : null}

      <Panel title={t('network.bluetooth.title')} description={t('network.bluetooth.copy')} action={<Bluetooth className="size-5 text-muted-foreground" />}>
        <ToggleField
          title={t('network.bluetooth.enabled')}
          description={t('network.bluetooth.enabledCopy')}
          control={<Switch checked={overview?.enabled ?? false} onCheckedChange={(value) => toggle({ enabled: value })} aria-label={t('network.bluetooth.enabled')} />}
        />
        <ToggleField
          title={t('network.bluetooth.discoverable')}
          description={t('network.bluetooth.discoverableCopy')}
          control={<Switch checked={overview?.discoverable ?? false} onCheckedChange={(value) => toggle({ discoverable: value })} aria-label={t('network.bluetooth.discoverable')} />}
        />
        <form className="grid gap-4" onSubmit={submit}>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField label={t('network.bluetooth.alias')} hint={t('network.bluetooth.aliasHint')}>
              {(id) => <Input id={id} value={draft.alias ?? overview?.adapter.alias ?? ''} onChange={(event) => setDraft({ ...draft, alias: event.target.value })} />}
            </FormField>
            <FormField label={t('network.bluetooth.pin')} hint={t('network.bluetooth.pinHint')}>
              {/* Shown, not masked: somebody has to type it on the other
                  device, which is the whole of what it is for. */}
              {(id) => <Input id={id} className="font-mono" inputMode="numeric" value={draft.pin ?? overview?.pin ?? ''} onChange={(event) => setDraft({ ...draft, pin: event.target.value })} />}
            </FormField>
          </div>
          <Button className="justify-self-end" type="submit" disabled={save.isPending || !overview}>
            {save.isPending ? <Spinner /> : null}{t('network.bluetooth.save')}
          </Button>
        </form>
      </Panel>

      <CollectionPanel
        title={t('network.bluetooth.devices')}
        action={(
          <Button size="sm" variant="outline" disabled={discovery.isPending || !overview?.enabled} onClick={() => discovery.mutate(!(overview?.adapter.discovering ?? false))}>
            <Radar />{overview?.adapter.discovering ? t('network.bluetooth.stopScan') : t('network.bluetooth.scan')}
          </Button>
        )}
      >
        <DataTable<BluetoothRow>
          rows={rows}
          rowKey={(row) => row.address}
          isPending={bluetooth.isPending}
          empty={t('network.bluetooth.empty')}
          columns={[
            { id: 'name', header: t('network.bluetooth.name'), cell: (row) => row.name || t('network.bluetooth.unnamed') },
            { id: 'address', header: t('network.bluetooth.address'), cell: (row) => <span className="font-mono text-[0.8125rem]">{row.address}</span> },
            { id: 'state', header: t('network.bluetooth.state'), cell: (row) => (
              <span className="flex flex-wrap gap-1">
                {row.declared ? <StatusBadge tone="success">{t('network.bluetooth.trusted')}</StatusBadge> : null}
                {row.connected ? <StatusBadge tone="success">{t('network.bluetooth.connected')}</StatusBadge> : null}
                {row.seen === false ? <StatusBadge tone="neutral">{t('network.bluetooth.outOfRange')}</StatusBadge> : null}
              </span>
            ) },
            { id: 'signal', header: t('network.bluetooth.signal'), cell: (row) => row.rssi === undefined ? '—' : `${row.rssi} dBm` },
            { id: 'actions', header: '', align: 'end', cell: (row) => (
              <span className="flex justify-end gap-2">
                {row.declared ? null : (
                  <Button type="button" size="sm" variant="outline" disabled={pair.isPending} onClick={() => pair.mutate(row.address)}>
                    {t('network.bluetooth.pair')}
                  </Button>
                )}
                {row.declared ? (
                  <ConfirmDialog
                    trigger={<Button type="button" size="icon-sm" variant="destructive" aria-label={t('network.bluetooth.remove', { address: row.address })}><Trash2 /></Button>}
                    title={t('network.bluetooth.remove', { address: row.address })}
                    description={t('network.bluetooth.removeCopy')}
                    confirmLabel={t('network.bluetooth.remove', { address: row.address })}
                    success={t('network.bluetooth.removed', { address: row.address })}
                    failure={t('network.bluetooth.remove', { address: row.address })}
                    onConfirm={() => remove.mutateAsync(row.address)}
                  />
                ) : null}
              </span>
            ) },
          ]}
        />
      </CollectionPanel>
    </div>
  )
}
