import { useEffect, useState, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Radar, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, WifiAccessPoint, WifiScan } from '@/lib/types'
import { band, groupScan, scanSecurity, type ScannedNetwork } from '@/lib/wifi-scan'
import { cn } from '@/shared/lib/utils'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FactList } from '@/shared/components/fact-list'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { PageSection } from '@/shared/components/page'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail, notifyFailure, notifySuccess } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { EditBar } from './change-bar'
import { inSubnet, isIPv4, overlaps, parseCidr, sameEntry, type IfaceEntry } from './model'
import { channelsFor, countryName, ELSEWHERE, EUROPE } from './regulatory'
import type { NetworkData } from './use-network'

interface WifiNetwork { ssid: string; psk?: string; hidden: boolean; priority: number }

interface Draft {
  enabled: boolean
  mode: 'client' | 'ap'
  start: 'always' | 'provisioning'
  ssid: string
  psk: string
  country: string
  countryConfirmed: boolean
  channel: number
  address: string
  /// The radio's own addressing as a client.
  addressing: IfaceEntry
  customDns: boolean
}

/// The radio: one role at a time, client or access point, set on one card
/// and saved together. Saved networks and the scan belong to the client; the
/// connected stations to the access point.
export function WifiTab({ data, onEditing }: { data: NetworkData; onEditing: (editing: boolean) => void }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const wifi = data.wifi
  const radio = wifi.radio ?? ''
  const ap = data.ap.data
  const client = data.client.data
  const stored = data.applied.ifaces[radio]?.entry ?? { dhcp: true }
  const [draft, setDraft] = useState<Draft>()
  const [confirming, setConfirming] = useState(false)
  useEffect(() => { onEditing(draft !== undefined); return () => onEditing(false) }, [draft, onEditing])

  const start = () => setDraft({
    enabled: wifi.mode !== 'off',
    mode: wifi.mode === 'ap' ? 'ap' : 'client',
    start: ap?.mode === 'provisioning' ? 'provisioning' : 'always',
    ssid: ap?.ssid ?? '',
    psk: '',
    country: (ap?.countryCode || 'DE').toUpperCase(),
    countryConfirmed: false,
    channel: ap?.channel ?? 6,
    address: ap?.address ?? '192.168.4.1/24',
    addressing: structuredClone(stored),
    customDns: Boolean(stored.dns?.length),
  })
  const errors = draft ? draftErrors(draft, data, t) : {}
  const errorCount = Object.keys(errors).length
  const modeChanged = draft !== undefined && draft.enabled && wifi.mode !== 'off' && draft.mode !== wifi.mode
  const turningOff = draft !== undefined && !draft.enabled && wifi.mode !== 'off'
  const onSession = data.session !== null && data.session === radio
  const dirty = draft !== undefined && (
    draft.enabled !== (wifi.mode !== 'off') || (draft.enabled && draft.mode !== wifi.mode)
    || (draft.mode === 'ap' && (draft.start !== (ap?.mode === 'provisioning' ? 'provisioning' : 'always') || draft.ssid !== (ap?.ssid ?? '') || draft.psk !== '' || draft.country !== (ap?.countryCode ?? '').toUpperCase() || draft.channel !== ap?.channel || draft.address !== ap?.address))
    || (draft.mode === 'client' && !sameEntry(draft.addressing, stored)))

  const saved = t(modeChanged ? 'network.wifi.switched' : turningOff ? 'network.wifi.turnedOff' : 'network.wifi.configSaved')
  const send = async () => {
    if (!draft) return
    const apBody = (mode: WifiAccessPoint['mode']): Partial<WifiAccessPoint> => {
      const body: Partial<WifiAccessPoint> = { ...ap, mode, interface: radio }
      // The stored key reads as the redaction sentinel; absent keeps it.
      delete body.psk
      return body
    }
    try {
      if (!draft.enabled) {
        if (ap && ap.mode !== 'off') await api<TaskAccepted>('/api/v1/wifi/ap', json('PUT', apBody('off')))
        if (client?.enabled) await api<TaskAccepted>('/api/v1/wifi/client', json('PUT', { enabled: false, interface: radio }))
      } else if (draft.mode === 'client') {
        // The access point lets go of the radio first: one radio, one role.
        if (ap && ap.mode !== 'off') await api<TaskAccepted>('/api/v1/wifi/ap', json('PUT', apBody('off')))
        if (!client?.enabled || client.interface !== radio) await api<TaskAccepted>('/api/v1/wifi/client', json('PUT', { enabled: true, interface: radio }))
        if (!sameEntry(draft.addressing, stored)) await api<TaskAccepted>(`/api/v1/network/${encodeURIComponent(radio)}`, json('PUT', draft.addressing))
      } else {
        if (client?.enabled) await api<TaskAccepted>('/api/v1/wifi/client', json('PUT', { enabled: false, interface: radio }))
        await api<TaskAccepted>('/api/v1/wifi/ap', json('PUT', {
          ...apBody(draft.start),
          ...(draft.ssid ? { ssid: draft.ssid } : {}),
          ...(draft.psk ? { psk: draft.psk } : {}),
          channel: draft.channel,
          countryCode: draft.country,
          address: draft.address,
        }))
      }
      setDraft(undefined)
    } finally {
      for (const key of ['wifi-client', 'wifi-ap', 'network', 'observed-network']) void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }
  const askOrSave = () => {
    if (modeChanged || turningOff || onSession) setConfirming(true)
    else void send().then(() => notifySuccess(saved), (error: unknown) => notifyFailure(t('network.wifi.saveFailed'), failureDetail(error, t('common.requestFailed'))))
  }
  const listMode = draft ? (draft.enabled ? draft.mode : undefined) : wifi.mode === 'off' ? 'client' : wifi.mode

  return (
    <>
      <PageSection title={t('network.wifi.state')}>
        <Panel><StateFacts data={data} /></Panel>
      </PageSection>

      {listMode === 'client' ? <ClientLists data={data} /> : null}
      {listMode === 'ap' && wifi.mode === 'ap' ? <Stations data={data} /> : null}

      <PageSection title={t('network.wifi.config')} description={draft ? t('network.wifi.editHint') : t('network.wifi.configCopy', { radio: radio || '—' })}>
        <Panel className={draft ? 'ring-2 ring-primary/40' : undefined}>
          {draft ? <ConfigForm draft={draft} setDraft={setDraft} errors={errors} current={wifi.mode} /> : (
            <>
              <div><Button size="sm" disabled={!radio} onClick={start}>{t('network.detail.edit')}</Button></div>
              <ConfigView data={data} />
            </>
          )}
        </Panel>
      </PageSection>

      {draft ? (
        <EditBar what={t('network.wifi.config')} dirty={dirty} errors={errorCount} onCancel={() => setDraft(undefined)} onDone={askOrSave} doneLabel={t('common.actions.save')} />
      ) : null}
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        tone={turningOff || onSession ? 'danger' : 'neutral'}
        title={t(turningOff ? 'network.wifi.confirmOff' : modeChanged ? (draft?.mode === 'ap' ? 'network.wifi.confirmAp' : 'network.wifi.confirmClient') : 'network.wifi.confirmSave')}
        description={(
          <span className="grid gap-2">
            {turningOff ? <span>{t(wifi.mode === 'ap' ? 'network.wifi.offApCopy' : 'network.wifi.offClientCopy', { radio })}</span> : null}
            {modeChanged ? <span>{t('network.wifi.switchCopy', { from: t(`network.wifi.modes.${wifi.mode}`) })}</span> : null}
            {modeChanged && draft?.mode === 'client' ? <span>{t('network.wifi.reachCopy', { host: `${data.hostname ?? 'mica'}.local` })}</span> : null}
            {onSession ? <span className="text-destructive">{t('network.wifi.sessionCopy')}</span> : null}
          </span>
        )}
        confirmLabel={t(turningOff ? 'network.wifi.turnOff' : modeChanged ? 'network.wifi.switchAndSave' : 'common.actions.save')}
        success={saved}
        failure={t('network.wifi.saveFailed')}
        onConfirm={send}
      />
    </>
  )
}

function draftErrors(draft: Draft, data: NetworkData, t: (key: string, params?: Record<string, unknown>) => string): Record<string, string> {
  const errors: Record<string, string> = {}
  if (!draft.enabled) return errors
  if (draft.mode === 'ap') {
    if (new TextEncoder().encode(draft.ssid).length > 32) errors.ssid = t('network.wifi.ssidLong')
    if (draft.psk && !(draft.psk.length >= 8 && draft.psk.length <= 63)) errors.psk = t('network.wifi.pskLength')
    if (!parseCidr(draft.address)) errors.address = t('network.problems.cidr')
    else {
      const clash = data.applied.order.find((name) => name !== data.wifi.radio && data.applied.ifaces[name].obs?.address && overlaps(draft.address, data.applied.ifaces[name].obs?.address ?? ''))
      if (clash) errors.address = t('network.problems.overlapWith', { name: clash })
    }
    if (!draft.countryConfirmed) errors.country = t('network.wifi.confirmCountry')
    if (!channelsFor(draft.country).includes(draft.channel)) errors.channel = t('network.wifi.channelInvalid')
    return errors
  }
  const entry = draft.addressing
  if (!entry.dhcp) {
    const address = entry.static?.address
    if (!address) errors.address = t('network.problems.addressMissing')
    else if (!parseCidr(address)) errors.address = t('network.problems.cidr')
    if (entry.static?.gateway && !inSubnet(entry.static.gateway, address)) errors.gateway = t('network.problems.gatewaySubnet')
  }
  const dns = entry.dhcp ? entry.dns : entry.static?.dns
  if ((dns ?? []).some((server) => !isIPv4(server))) errors.dns = t('network.problems.dns', { list: (dns ?? []).filter((server) => !isIPv4(server)).join(', ') })
  return errors
}

function StateFacts({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const wifi = data.wifi
  const radio = wifi.radio
  const address = radio ? data.applied.ifaces[radio]?.obs?.address : undefined
  if (wifi.mode === 'off') return <FactList facts={[{ id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.off')}</strong> }]} />
  if (wifi.mode === 'client') {
    return <FactList facts={[
      { id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.modes.client')}</strong> },
      { id: 'state', label: t('network.fields.state'), value: wifi.association ? <span className="text-success">{t('network.status.connected')}</span> : <span className="text-warning">{t('network.wifi.searching')}</span> },
      { id: 'ssid', label: 'SSID', value: wifi.association?.ssid ?? '—' },
      { id: 'signal', label: t('network.wifi.signal'), value: wifi.association?.rssiDbm !== undefined ? `${wifi.association.rssiDbm} dBm` : '—' },
      { id: 'address', label: t('network.fields.address'), value: address ?? '—', mono: true },
    ]} />
  }
  return <FactList facts={[
    { id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.modes.ap')}</strong> },
    { id: 'state', label: t('network.fields.state'), value: wifi.apRunning ? <span className="text-success">{t('network.status.apRunning')}</span> : <span className="text-destructive">{t('network.status.apDown')}</span> },
    { id: 'ssid', label: 'SSID', value: <strong>{wifi.apSsid || t('network.wifi.ssidAuto')}</strong> },
    { id: 'channel', label: t('network.wifi.channel'), value: data.ap.data ? t('network.wifi.channelValue', { channel: data.ap.data.channel }) : '—' },
    { id: 'clients', label: t('network.wifi.clients'), value: t('network.wifi.clientCount', { count: wifi.stations }) },
  ]} />
}

function ConfigView({ data }: { data: NetworkData }) {
  const { t, i18n } = useTranslation()
  const wifi = data.wifi
  const ap = data.ap.data
  const entry = (wifi.radio ? data.applied.ifaces[wifi.radio]?.entry : undefined) ?? { dhcp: true }
  const facts: Parameters<typeof FactList>[0]['facts'] = [
    { id: 'enabled', label: t('network.wifi.enabled'), value: wifi.mode === 'off' ? t('network.wifi.offRadio', { radio: wifi.radio ?? '' }) : <span className="text-success">{t('network.values.on')}</span> },
    wifi.mode !== 'off' && { id: 'mode', label: t('network.wifi.mode'), value: t(`network.wifi.modes.${wifi.mode}`) },
  ]
  if (wifi.mode === 'ap' && ap) {
    facts.push(
      { id: 'start', label: t('network.wifi.start'), value: t(`network.wifi.starts.${ap.mode === 'provisioning' ? 'provisioning' : 'always'}`) },
      { id: 'ssid', label: 'SSID', value: ap.ssid || t('network.wifi.ssidAuto') },
      { id: 'security', label: t('network.wifi.security'), value: 'WPA2-PSK' },
      { id: 'psk', label: t('network.wifi.password'), value: t('network.wifi.pskHidden') },
      { id: 'country', label: t('network.wifi.country'), value: `${countryName(ap.countryCode, i18n.resolvedLanguage ?? 'en')} (${ap.countryCode})` },
      { id: 'channel', label: t('network.wifi.channel'), value: t('network.wifi.channelValue', { channel: ap.channel }) },
      { id: 'address', label: t('network.wifi.apAddress'), value: ap.address, mono: true },
      { id: 'dhcp', label: t('network.wifi.apDhcp'), value: t('network.wifi.apDhcpCopy') },
    )
  }
  if (wifi.mode === 'client') {
    facts.push(
      { id: 'preset', label: t('network.detail.preset'), value: t(entry.dhcp ? 'network.presets.dhcp' : 'network.presets.static') },
      ...(entry.dhcp
        ? [{ id: 'dns', label: t('network.fields.dns'), value: entry.dns?.length ? entry.dns.join(', ') : t('network.detail.leaseDns') }]
        : [
            { id: 'address', label: t('network.fields.address'), value: entry.static?.address || '—', mono: true },
            { id: 'gateway', label: t('network.fields.gateway'), value: entry.static?.gateway || t('network.values.none'), mono: true },
            { id: 'dns', label: t('network.fields.dns'), value: entry.static?.dns?.length ? entry.static.dns.join(', ') : t('network.values.none'), mono: true },
          ]),
    )
  }
  return <FactList facts={facts} />
}

function ConfigForm({ draft, setDraft, errors, current }: { draft: Draft; setDraft: (draft: Draft) => void; errors: Record<string, string>; current: 'off' | 'client' | 'ap' }) {
  const { t, i18n } = useTranslation()
  const locale = i18n.resolvedLanguage ?? 'en'
  const hint = (key: string, fallback?: ReactNode) => errors[key] ? <span className="text-destructive">{errors[key]}</span> : fallback
  const countries = [...EUROPE, ...ELSEWHERE.filter((code) => !EUROPE.includes(code))]
  if (!countries.includes(draft.country)) countries.unshift(draft.country)
  const entry = draft.addressing
  const setEntry = (next: IfaceEntry) => setDraft({ ...draft, addressing: next })
  return (
    <div className="grid gap-5">
      <ToggleField
        title={t('network.wifi.enabled')}
        description={draft.enabled ? undefined : t('network.wifi.offHint')}
        control={<Switch checked={draft.enabled} onCheckedChange={(enabled) => setDraft({ ...draft, enabled })} aria-label={t('network.wifi.enabled')} />}
      />
      {draft.enabled ? (
        <FormField label={t('network.wifi.mode')} hint={current !== 'off' && draft.mode !== current ? t('network.wifi.switchHint', { from: t(`network.wifi.modes.${current}`) }) : undefined}>
          {() => <SegmentedControl<'client' | 'ap'> label={t('network.wifi.mode')} value={draft.mode} onValueChange={(mode) => setDraft({ ...draft, mode })} segments={[{ value: 'client', label: t('network.wifi.modes.client') }, { value: 'ap', label: t('network.wifi.modes.ap') }]} />}
        </FormField>
      ) : null}
      {!draft.enabled ? null : draft.mode === 'ap' ? (
        <>
          <FormField label={t('network.wifi.start')}>
            {() => <SegmentedControl<'always' | 'provisioning'> label={t('network.wifi.start')} value={draft.start} onValueChange={(start) => setDraft({ ...draft, start })} segments={[{ value: 'always', label: t('network.wifi.starts.always') }, { value: 'provisioning', label: t('network.wifi.starts.provisioning') }]} />}
          </FormField>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField label="SSID" hint={hint('ssid', t('network.wifi.ssidHint'))}>
              {(id) => <Input id={id} value={draft.ssid} aria-invalid={Boolean(errors.ssid)} onChange={(event) => setDraft({ ...draft, ssid: event.target.value })} />}
            </FormField>
            <FormField label={t('network.wifi.password')} hint={hint('psk', t('network.wifi.pskHint'))}>
              {(id) => <Input id={id} type="password" value={draft.psk} placeholder={t('network.wifi.pskKeep')} aria-invalid={Boolean(errors.psk)} onChange={(event) => setDraft({ ...draft, psk: event.target.value })} />}
            </FormField>
          </div>
          <FormField label={t('network.wifi.country')} hint={hint('country', t('network.wifi.countryHint'))}>
            {(id) => (
              <div className="grid gap-2">
                <Select value={draft.country} onValueChange={(value) => { const country = String(value); setDraft({ ...draft, country, channel: channelsFor(country).includes(draft.channel) ? draft.channel : 1 }) }}>
                  <SelectTrigger id={id} className="w-72"><SelectValue>{`${countryName(draft.country, locale)} (${draft.country})`}</SelectValue></SelectTrigger>
                  <SelectContent>{countries.map((code) => <SelectItem key={code} value={code}>{countryName(code, locale)} ({code})</SelectItem>)}</SelectContent>
                </Select>
                <label className="flex items-center gap-2 text-sm">
                  <input type="checkbox" className="size-4 accent-primary" checked={draft.countryConfirmed} onChange={(event) => setDraft({ ...draft, countryConfirmed: event.target.checked })} />
                  {t('network.wifi.countryConfirm')}
                </label>
              </div>
            )}
          </FormField>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField label={t('network.wifi.channel')} hint={hint('channel', t('network.wifi.channelHint'))}>
              {(id) => (
                <Select value={String(draft.channel)} onValueChange={(value) => setDraft({ ...draft, channel: Number(value) })}>
                  <SelectTrigger id={id} className="w-48 font-mono"><SelectValue /></SelectTrigger>
                  <SelectContent>{channelsFor(draft.country).map((channel) => <SelectItem key={channel} value={String(channel)}>{t('network.wifi.channelValue', { channel })}</SelectItem>)}</SelectContent>
                </Select>
              )}
            </FormField>
            <FormField label={t('network.wifi.apAddress')} hint={hint('address', t('network.wifi.apAddressHint'))}>
              {(id) => <Input id={id} className="font-mono" value={draft.address} aria-invalid={Boolean(errors.address)} onChange={(event) => setDraft({ ...draft, address: event.target.value.trim() })} />}
            </FormField>
          </div>
          <p className="text-sm text-muted-foreground">{t('network.wifi.securityNote')}</p>
        </>
      ) : (
        <>
          <FormField label={t('network.detail.preset')}>
            {() => <SegmentedControl<'dhcp' | 'static'> label={t('network.detail.preset')} value={entry.dhcp ? 'dhcp' : 'static'} onValueChange={(preset) => setEntry(preset === 'dhcp' ? { ...withoutAddressing(entry), dhcp: true } : { ...withoutAddressing(entry), dhcp: false, static: { address: entry.static?.address ?? '', ...(entry.static?.gateway ? { gateway: entry.static.gateway } : {}), dns: entry.dns ?? entry.static?.dns ?? [] } })} segments={[{ value: 'dhcp', label: t('network.presets.dhcp') }, { value: 'static', label: t('network.presets.static') }]} />}
          </FormField>
          {entry.dhcp ? (
            <>
              <ToggleField
                title={t('network.fields.customDns')}
                description={t('network.detail.customDnsHint')}
                control={<Switch checked={draft.customDns} onCheckedChange={(on) => setDraft({ ...draft, customDns: on, addressing: on ? entry : { ...entry, dns: [] } })} aria-label={t('network.fields.customDns')} />}
              />
              {draft.customDns ? (
                <FormField label={t('network.fields.dns')} hint={hint('dns', t('network.detail.listHint'))}>
                  {(id) => <Input id={id} className="font-mono" defaultValue={(entry.dns ?? []).join(', ')} onChange={(event) => setEntry({ ...entry, dns: split(event.target.value) })} />}
                </FormField>
              ) : null}
            </>
          ) : (
            <div className="grid gap-4 sm:grid-cols-2">
              <FormField label={t('network.fields.address')} hint={hint('address', t('network.detail.cidrHint'))}>
                {(id) => <Input id={id} className="font-mono" value={entry.static?.address ?? ''} placeholder="192.168.31.20/24" onChange={(event) => setEntry({ ...entry, static: { ...entry.static, address: event.target.value.trim() } })} />}
              </FormField>
              <FormField label={t('network.fields.gateway')} hint={hint('gateway')}>
                {(id) => <Input id={id} className="font-mono" value={entry.static?.gateway ?? ''} placeholder={t('network.values.none')} onChange={(event) => { const gateway = event.target.value.trim(); const next = { ...entry, static: { address: entry.static?.address ?? '', ...entry.static } }; if (gateway) next.static.gateway = gateway; else delete next.static.gateway; setEntry(next) }} />}
              </FormField>
              <FormField className="sm:col-span-2" label={t('network.fields.dns')} hint={hint('dns', t('network.detail.listHint'))}>
                {(id) => <Input id={id} className="font-mono" defaultValue={(entry.static?.dns ?? []).join(', ')} onChange={(event) => setEntry({ ...entry, static: { address: entry.static?.address ?? '', ...entry.static, dns: split(event.target.value) } })} />}
              </FormField>
            </div>
          )}
        </>
      )}
    </div>
  )
}

const split = (text: string) => text.split(/[,，\s]+/).map((item) => item.trim()).filter(Boolean)

function withoutAddressing(entry: IfaceEntry): IfaceEntry {
  const next = { ...entry }
  delete next.static
  delete next.dns
  delete next.dhcpServer
  return next
}

/// The client's saved networks and the radio's scan.
function ClientLists({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const networks = useQuery({ queryKey: ['wifi-networks'], queryFn: () => api<WifiNetwork[]>('/api/v1/wifi/client/networks') })
  const [dialog, setDialog] = useState<{ ssid: string; existing: boolean; secured: boolean }>()
  const [psk, setPsk] = useState('')
  const [hidden, setHidden] = useState(false)
  const [priority, setPriority] = useState('0')
  const [ssid, setSsid] = useState('')
  const wifi = data.wifi
  const scanBlocked = wifi.mode === 'ap' ? t('network.wifi.scanApHolds', { iface: wifi.radio ?? '' }) : wifi.mode === 'off' ? t('network.wifi.scanClientOff') : undefined
  const scan = useMutationFeedback<WifiScan>({
    mutationFn: () => api<WifiScan>('/api/v1/wifi/client/scan', { method: 'POST' }),
    success: t('network.wifi.scanned'),
    failure: t('network.wifi.scan'),
  })
  const remove = useMutationFeedback<void, string>({
    mutationFn: (name) => api<void>(`/api/v1/wifi/client/networks/${encodeURIComponent(name)}`, { method: 'DELETE' }),
    success: (_data, name) => t('network.wifi.removed', { name }),
    failure: t('network.wifi.remove'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wifi-networks'] }),
  })
  const open = (next: { ssid: string; existing: boolean; secured: boolean }, entry?: WifiNetwork) => {
    setDialog(next); setSsid(next.ssid); setPsk(''); setHidden(entry?.hidden ?? false); setPriority(String(entry?.priority ?? 0))
  }
  const pskError = psk && !(psk.length >= 8 && psk.length <= 63) ? t('network.wifi.pskLength') : undefined
  const ssidError = !dialog?.existing && ssid && networks.data?.some((each) => each.ssid === ssid) ? t('network.wifi.ssidSaved') : undefined
  const submit = () => {
    if (!dialog) return Promise.resolve()
    const body = { ssid, ...(psk ? { psk } : {}), hidden, priority: Number(priority) }
    const request = dialog.existing
      ? api<WifiNetwork>(`/api/v1/wifi/client/networks/${encodeURIComponent(dialog.ssid)}`, json('PUT', body))
      : api<WifiNetwork>('/api/v1/wifi/client/networks', json('POST', body))
    return request.then(() => queryClient.invalidateQueries({ queryKey: ['wifi-networks'] }))
  }
  const signal = (network: ScannedNetwork) => network.best.signalDbm === undefined ? '—' : `${network.best.signalDbm} dBm`

  return (
    <>
      <PageSection title={t('network.wifi.savedNetworks')} description={t('network.wifi.savedCopy')}>
        {networks.error ? <Callout tone="danger" title={failureDetail(networks.error, t('common.requestFailed'))} /> : null}
        <CollectionPanel action={<Button size="sm" variant="outline" onClick={() => open({ ssid: '', existing: false, secured: true })}><Plus />{t('network.wifi.add')}</Button>} title={t('network.wifi.savedNetworks')}>
          <DataTable<WifiNetwork>
            rows={networks.data ? [...networks.data].sort((left, right) => right.priority - left.priority) : undefined}
            rowKey={(entry) => entry.ssid}
            isPending={networks.isPending}
            empty={t('network.wifi.empty')}
            columns={[
              { id: 'ssid', header: t('network.wifi.network'), cell: (entry) => <span className="flex flex-wrap items-center gap-1.5"><strong>{entry.ssid}</strong>{entry.hidden ? <span className="text-xs text-muted-foreground">{t('network.wifi.hidden')}</span> : null}{wifi.association?.ssid === entry.ssid ? <span className="text-xs text-primary">{t('network.status.connected')}</span> : null}</span> },
              { id: 'security', header: t('network.wifi.security'), cell: (entry) => t(entry.psk ? 'network.wifi.wpa' : 'network.wifi.open') },
              { id: 'priority', header: t('network.wifi.priority'), cell: (entry) => entry.priority },
              { id: 'actions', header: '', align: 'end', cell: (entry) => (
                <span className="flex justify-end gap-2">
                  <Button type="button" size="sm" variant="outline" onClick={() => open({ ssid: entry.ssid, existing: true, secured: Boolean(entry.psk) }, entry)}>{t('common.actions.edit')}</Button>
                  <ConfirmDialog
                    trigger={<Button type="button" size="sm" variant="destructive" aria-label={t('network.wifi.removeNamed', { name: entry.ssid })}><Trash2 /></Button>}
                    title={t('network.wifi.removeNamed', { name: entry.ssid })}
                    description={t('network.wifi.removeCopy')}
                    confirmLabel={t('network.wifi.remove')}
                    success={t('network.wifi.removed', { name: entry.ssid })}
                    failure={t('network.wifi.remove')}
                    onConfirm={() => remove.mutateAsync(entry.ssid)}
                  />
                </span>
              ) },
            ]}
          />
        </CollectionPanel>
      </PageSection>

      <PageSection title={t('network.wifi.nearby')} description={t('network.wifi.nearbyCopy')}>
        <CollectionPanel
          title={t('network.wifi.nearby')}
          action={<Button size="sm" variant="outline" disabled={Boolean(scanBlocked) || scan.isPending} onClick={() => scan.mutate()}><Radar />{scan.isPending ? t('network.wifi.scanning') : t('network.wifi.scan')}</Button>}
        >
          {scanBlocked ? <p className="p-4 text-sm text-muted-foreground">{scanBlocked}</p> : (
            <>
              {scan.data && scan.data.available === false ? <p className="p-4 text-sm text-warning">{scan.data.reason === 'access-point' ? t('network.wifi.scanApHolds', { iface: scan.data.interface ?? wifi.radio ?? '' }) : scan.data.reason === 'client-off' ? t('network.wifi.scanClientOff') : scan.data.detail ?? t('network.wifi.scanUnavailable')}</p> : null}
              <DataTable<ScannedNetwork>
                rows={scan.data?.networks === undefined ? [] : groupScan(scan.data.networks)}
                rowKey={(network) => network.best.bssid}
                isPending={scan.isPending}
                empty={t('network.wifi.scanEmpty')}
                columns={[
                  { id: 'ssid', header: t('network.wifi.network'), cell: (network) => <span className="flex flex-wrap items-center gap-1.5">{network.ssid ? <strong>{network.ssid}</strong> : <span className="text-muted-foreground">{t('network.wifi.hiddenNetwork')}</span>}{networks.data?.some((each) => each.ssid === network.ssid) ? <span className="text-xs text-muted-foreground">{t('network.wifi.alreadySaved')}</span> : null}</span> },
                  { id: 'signal', header: t('network.wifi.signal'), cell: signal },
                  { id: 'security', header: t('network.wifi.security'), cell: (network) => t(`network.wifi.securities.${scanSecurity(network.best.flags)}`) },
                  { id: 'band', header: t('network.wifi.band'), cell: (network) => [...new Set(network.accessPoints.map((bss) => band(bss.frequencyMhz)).filter(Boolean))].map((each) => `${each} GHz`).join(' / ') || '—' },
                  { id: 'actions', header: '', align: 'end', cell: (network) => {
                    const security = scanSecurity(network.best.flags)
                    const saved = networks.data?.some((each) => each.ssid === network.ssid)
                    return saved || security === 'eap' || security === 'wep' ? null : <Button type="button" size="sm" variant="outline" onClick={() => open({ ssid: network.ssid, existing: false, secured: security !== 'open' })}>{t('network.wifi.connect')}</Button>
                  } },
                ]}
              />
            </>
          )}
        </CollectionPanel>
      </PageSection>

      <FormDialog
        open={dialog !== undefined}
        onOpenChange={(next) => { if (!next) setDialog(undefined) }}
        title={dialog?.existing ? t('network.wifi.editNamed', { name: dialog.ssid }) : t('network.wifi.add')}
        description={t(dialog?.existing ? 'network.wifi.editCopy' : 'network.wifi.addCopy')}
        submitLabel={t('common.actions.save')}
        success={t('network.wifi.networkSaved', { name: ssid })}
        failure={t('network.wifi.add')}
        onSubmit={submit}
        submitDisabled={!ssid || Boolean(pskError) || Boolean(ssidError) || (!dialog?.existing && dialog?.secured === true && !psk)}
      >
        <FormField label={t('network.wifi.network')} hint={ssidError ? <span className="text-destructive">{ssidError}</span> : undefined}>
          {(id) => <Input id={id} value={ssid} disabled={dialog?.existing} onChange={(event) => setSsid(event.target.value)} required />}
        </FormField>
        <FormField label={t('network.wifi.security')}>
          {() => <SegmentedControl<'psk' | 'open'> label={t('network.wifi.security')} value={dialog?.secured === false ? 'open' : 'psk'} onValueChange={(value) => { if (dialog) setDialog({ ...dialog, secured: value === 'psk' }); if (value === 'open') setPsk('') }} segments={[{ value: 'psk', label: t('network.wifi.wpa') }, { value: 'open', label: t('network.wifi.open') }]} />}
        </FormField>
        {dialog?.secured !== false ? (
          <FormField label={t('network.wifi.password')} hint={pskError ? <span className="text-destructive">{pskError}</span> : t(dialog?.existing ? 'network.wifi.keepKey' : 'network.wifi.pskLength')}>
            {(id) => <Input id={id} type="password" value={psk} placeholder={dialog?.existing ? t('network.wifi.pskKeep') : ''} onChange={(event) => setPsk(event.target.value)} />}
          </FormField>
        ) : null}
        <ToggleField title={t('network.wifi.hidden')} description={t('network.wifi.hiddenHint')} control={<Switch checked={hidden} onCheckedChange={setHidden} aria-label={t('network.wifi.hidden')} />} />
        <FormField label={t('network.wifi.priority')} hint={t('network.wifi.priorityHint')}>
          {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={priority} onChange={(event) => setPriority(event.target.value.replace(/\D/g, ''))} />}
        </FormField>
      </FormDialog>
    </>
  )
}

/// Who is connected to the access point.
function Stations({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const stations = (data.status.data?.accessPoint?.entries ?? []).find((entry) => entry.interface === data.wifi.radio)?.stations ?? []
  const bytes = (value?: number) => value === undefined ? '—' : value >= 1e9 ? `${(value / 1e9).toFixed(1)} GB` : value >= 1e6 ? `${(value / 1e6).toFixed(1)} MB` : `${Math.round(value / 1e3)} KB`
  return (
    <PageSection title={t('network.wifi.stations')} description={t('network.wifi.stationsCopy')}>
      <CollectionPanel>
        <DataTable
          rows={stations}
          rowKey={(station) => station.mac}
          empty={t('network.wifi.noStations')}
          columns={[
            { id: 'mac', header: 'MAC', cell: (station) => <span className="font-mono text-[0.8125rem]">{station.mac}</span> },
            { id: 'signal', header: t('network.wifi.signal'), cell: (station) => station.signalDbm !== undefined ? `${station.signalDbm} dBm` : '—' },
            { id: 'connected', header: t('network.wifi.connectedFor'), cell: (station) => station.connectedSeconds !== undefined ? t('network.wifi.minutes', { count: Math.floor(station.connectedSeconds / 60) }) : '—' },
            { id: 'traffic', header: t('network.wifi.traffic'), cell: (station) => <span className={cn('text-muted-foreground')}>{bytes(station.rxBytes)} / {bytes(station.txBytes)}</span> },
          ]}
        />
      </CollectionPanel>
    </PageSection>
  )
}
