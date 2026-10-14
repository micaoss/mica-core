import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { RadioTower } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { ObservedNetworkState, TaskAccepted, WifiAccessPoint } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

const MODES = ['off', 'provisioning', 'always'] as const

/// The device's own access point: when it runs, which radio it runs on, and
/// what it advertises.
///
/// The key is never shown -- a read substitutes the redaction sentinel -- so
/// the field is empty and an empty field keeps the stored key. That is the
/// same rule the known-network editor takes, for the same reason.
export function WifiApPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const ap = useQuery({ queryKey: ['wifi-ap'], queryFn: () => api<WifiAccessPoint>('/api/v1/wifi/ap') })
  const status = useQuery({ queryKey: ['observed-network'], queryFn: () => api<ObservedNetworkState>('/api/v1/network/status'), retry: false })
  const [draft, setDraft] = useState<Partial<WifiAccessPoint>>({})
  const [psk, setPsk] = useState('')

  const radios = status.data?.capabilities.wifi.interfaces ?? []
  // Optional through the member itself: a daemon older than the access-point
  // observation answers a document without it, and a console that read
  // `.entries` off that would take the page down rather than show one fact
  // less.
  const stations = (status.data?.accessPoint?.entries ?? []).reduce((count, entry) => count + entry.stationCount, 0)
  const current: WifiAccessPoint | undefined = ap.data && { ...ap.data, ...draft }

  const save = useMutationFeedback<TaskAccepted, WifiAccessPoint>({
    mutationFn: (body) => api<TaskAccepted>('/api/v1/wifi/ap', json('PUT', body)),
    success: t('network.ap.saved'),
    failure: t('network.ap.save'),
    onSuccess: () => {
      setPsk('')
      void queryClient.invalidateQueries({ queryKey: ['wifi-ap'] })
    },
  })

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!current) return
    // The stored key is dropped from the body: it was read as the redaction
    // sentinel, and sending that back is refused. An empty field means keep.
    const body = { ...current }
    delete body.psk
    save.mutate({ ...body, ...(psk ? { psk } : {}) })
  }

  return (
    <Panel
      title={t('network.ap.title')}
      description={t('network.ap.copy')}
      action={<RadioTower className="size-5 text-muted-foreground" />}
    >
      {ap.error ? <Callout tone="danger" title={failureDetail(ap.error, t('common.requestFailed'))} /> : null}
      {current?.mode !== 'off' && stations > 0 ? <Callout tone="neutral" title={t('network.ap.connected', { count: stations })} /> : null}
      <form className="grid gap-4" onSubmit={submit}>
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('network.ap.mode')} hint={t(`network.ap.modes.${current?.mode ?? 'off'}`)}>
            {(id) => (
              <Select value={current?.mode ?? 'off'} onValueChange={(value) => setDraft({ ...draft, mode: String(value) as WifiAccessPoint['mode'] })}>
                <SelectTrigger id={id} aria-label={t('network.ap.mode')}><SelectValue /></SelectTrigger>
                <SelectContent>{MODES.map((mode) => <SelectItem value={mode} key={mode}>{t(`network.ap.modeNames.${mode}`)}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
          <FormField label={t('network.ap.radio')}>
            {(id) => radios.length > 1 ? (
              <Select value={current?.interface ?? ''} onValueChange={(value) => setDraft({ ...draft, interface: String(value) })}>
                <SelectTrigger id={id} aria-label={t('network.ap.radio')}><SelectValue /></SelectTrigger>
                <SelectContent>{radios.map((name) => <SelectItem value={name} key={name}>{name}</SelectItem>)}</SelectContent>
              </Select>
            ) : (
              <Input id={id} className="font-mono" value={current?.interface ?? ''} onChange={(event) => setDraft({ ...draft, interface: event.target.value })} required />
            )}
          </FormField>
          <FormField label="SSID" hint={t('network.ap.ssidHint')}>
            {(id) => <Input id={id} value={current?.ssid ?? ''} onChange={(event) => setDraft({ ...draft, ssid: event.target.value })} />}
          </FormField>
          <FormField label={t('network.ap.psk')} hint={t('network.ap.pskHint')}>
            {(id) => <Input id={id} type="password" minLength={8} value={psk} placeholder={t('network.ap.pskPlaceholder')} onChange={(event) => setPsk(event.target.value)} />}
          </FormField>
          <FormField label={t('network.ap.channel')}>
            {(id) => <Input id={id} type="number" min={1} max={14} value={current?.channel ?? 6} onChange={(event) => setDraft({ ...draft, channel: Number(event.target.value) })} required />}
          </FormField>
          <FormField label={t('network.ap.country')}>
            {(id) => <Input id={id} maxLength={2} value={current?.countryCode ?? ''} onChange={(event) => setDraft({ ...draft, countryCode: event.target.value.toUpperCase() })} required />}
          </FormField>
          <FormField className="sm:col-span-2" label={t('network.ap.address')} hint={t('network.ap.addressHint')}>
            {(id) => <Input id={id} className="font-mono" value={current?.address ?? ''} onChange={(event) => setDraft({ ...draft, address: event.target.value })} required />}
          </FormField>
        </div>
        <Button className="justify-self-end" type="submit" disabled={save.isPending || !current}>
          {save.isPending ? <Spinner /> : null}{t('network.ap.save')}
        </Button>
      </form>
    </Panel>
  )
}
