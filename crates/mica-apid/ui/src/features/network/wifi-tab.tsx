import { useEffect, useState, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { Plus, Radar, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, WifiAccessPoint, WifiChannels, WifiScan } from '@/lib/types'
import { groupScan, scanSecurity, type ScannedNetwork } from '@/lib/wifi-scan'
import { cn } from '@/shared/lib/utils'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FactList } from '@/shared/components/fact-list'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail, notifyFailure, notifySuccess } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { provisioningQuery } from './bluetooth'
import { EditBar } from './change-bar'
import { inSubnet, intToIp, ipToInt, isIPv4, overlaps, parseCidr, sameEntry, type IfaceEntry } from './model'
import { Group, Section, useWifiText } from './parts'
import { countryName, ELSEWHERE, EUROPE } from './regulatory'
import type { NetworkData } from './use-network'

interface WifiNetwork { ssid: string; psk?: string; hidden: boolean; priority: number; autoConnect?: boolean }

type ApSecurity = NonNullable<WifiAccessPoint['security']>
const AP_SECURITIES: ApSecurity[] = ['wpa2-psk', 'wpa2-wpa3', 'open']
const MIN_LEASE = 60

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
  security: ApSecurity
  poolStart: string
  poolEnd: string
  /// Seconds; empty is the DHCP server's own default.
  lease: string
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
  // Where the running access point's key comes from: `derived` is the key on
  // the label, which applies until the user sets one here.
  const apState = useQuery({ queryKey: ['state', 'wifiAp'], queryFn: () => api<{ pskSource?: string | null }>('/api/v1/state/wifiAp'), enabled: wifi.mode === 'ap', retry: false })
  useEffect(() => { onEditing(draft !== undefined); return () => onEditing(false) }, [draft, onEditing])
  // A BLE provisioning session holds the radio: the device refuses Wi-Fi
  // writes (423) until it ends.
  const provisioning = useQuery({ ...provisioningQuery, enabled: data.serves('bluetooth'), refetchInterval: 5_000 })
  const locked = provisioning.data?.wifiLocked === true

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
    security: ap?.security ?? 'wpa2-psk',
    ...(ap?.dhcpServer ? { poolStart: ap.dhcpServer.poolStart, poolEnd: ap.dhcpServer.poolEnd } : derivedPool(ap?.address ?? '192.168.4.1/24')),
    lease: ap?.dhcpServer?.leaseSeconds ? String(ap.dhcpServer.leaseSeconds) : '',
    addressing: structuredClone(stored),
    customDns: Boolean(stored.dns?.length),
  })
  const channels = useQuery({
    queryKey: ['wifi-channels', draft?.country],
    queryFn: () => api<WifiChannels>(`/api/v1/wifi/channels?country=${encodeURIComponent(draft?.country ?? '')}`),
    enabled: draft !== undefined && draft.mode === 'ap',
    retry: false,
  })
  const allowed = channels.data ? [...(channels.data.auto ? [0] : []), ...channels.data.channels] : undefined
  const errors = draft ? draftErrors(draft, data, allowed, t) : {}
  const errorCount = Object.keys(errors).length
  const modeChanged = draft !== undefined && draft.enabled && wifi.mode !== 'off' && draft.mode !== wifi.mode
  const turningOff = draft !== undefined && !draft.enabled && wifi.mode !== 'off'
  const onSession = data.session !== null && data.session === radio
  const openNow = draft !== undefined && draft.enabled && draft.mode === 'ap' && draft.security === 'open' && !(wifi.mode === 'ap' && ap?.security === 'open')
  const apChanged = draft !== undefined && ap !== undefined && JSON.stringify(apFields(draft)) !== JSON.stringify(storedFields(ap))
  const dirty = draft !== undefined && (
    draft.enabled !== (wifi.mode !== 'off') || (draft.enabled && draft.mode !== wifi.mode)
    || (draft.mode === 'ap' && (draft.start !== (ap?.mode === 'provisioning' ? 'provisioning' : 'always') || draft.psk !== '' || apChanged))
    || (draft.mode === 'client' && !sameEntry(draft.addressing, stored)))

  const saved = t(modeChanged ? 'network.wifi.switched' : turningOff ? 'network.wifi.turnedOff' : 'network.wifi.configSaved')
  const send = async () => {
    if (!draft) return
    // The role and its configuration are one write: one radio, one role.
    const role = (body: object) => api<TaskAccepted>('/api/v1/wifi', json('PUT', body))
    try {
      if (!draft.enabled) {
        await role({ enabled: false, mode: draft.mode })
      } else if (draft.mode === 'client') {
        if (client && client.interface !== radio) await api<TaskAccepted>('/api/v1/wifi/client', json('PUT', { enabled: client.enabled, interface: radio }))
        await role({ enabled: true, mode: 'client', ...(sameEntry(draft.addressing, stored) ? {} : { client: { addressing: draft.addressing } }) })
      } else {
        // The stored key reads as the redaction sentinel; absent keeps it.
        const config: Record<string, unknown> = { start: draft.start, interface: radio, holdDownSeconds: ap?.holdDownSeconds ?? 120, graceSeconds: ap?.graceSeconds ?? 60, ...apFields(draft), countryConfirmed: draft.countryConfirmed }
        if (!config.ssid) delete config.ssid
        if (draft.psk && draft.security !== 'open') config.psk = draft.psk
        await role({ enabled: true, mode: 'ap', ap: config })
      }
      setDraft(undefined)
    } finally {
      for (const key of ['wifi-client', 'wifi-ap', 'network', 'observed-network']) void queryClient.invalidateQueries({ queryKey: [key] })
    }
  }
  const askOrSave = () => {
    if (modeChanged || turningOff || onSession || openNow) setConfirming(true)
    else void send().then(() => notifySuccess(saved), (error: unknown) => notifyFailure(t('network.wifi.saveFailed'), failureDetail(error, t('common.requestFailed'))))
  }
  const listMode = draft ? (draft.enabled ? draft.mode : undefined) : wifi.mode === 'off' ? 'client' : wifi.mode

  return (
    <>
      {locked ? <Callout tone="warning" title={t('network.wifi.locked')} /> : null}
      {wifi.mode === 'ap' && apState.data?.pskSource === 'derived' ? <Callout tone="warning" title={t('network.wifi.labelKey')} /> : null}
      <Section title={t('network.wifi.state')}>
        <StateFacts data={data} />
      </Section>

      {listMode === 'client' ? <ClientLists data={data} locked={locked} /> : null}
      {listMode === 'ap' && wifi.mode === 'ap' ? <Stations data={data} /> : null}

      <Section
        title={t('network.wifi.config')}
        description={draft ? t('network.wifi.editHint') : t('network.wifi.configCopy', { radio: radio || '—' })}
        editing={draft !== undefined}
        action={draft ? undefined : <Button size="sm" disabled={!radio || locked} onClick={start}>{t('network.detail.edit')}</Button>}
      >
        {draft ? <ConfigForm draft={draft} setDraft={setDraft} errors={errors} current={wifi.mode} allowed={allowed} /> : <ConfigView data={data} />}
      </Section>

      {draft ? (
        <EditBar what={t('network.wifi.config')} dirty={dirty} errors={errorCount} onCancel={() => setDraft(undefined)} onDone={askOrSave} doneLabel={t('common.actions.save')} />
      ) : null}
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        tone={turningOff || onSession || openNow ? 'danger' : 'neutral'}
        title={t(turningOff ? 'network.wifi.confirmOff' : modeChanged ? (draft?.mode === 'ap' ? 'network.wifi.confirmAp' : 'network.wifi.confirmClient') : 'network.wifi.confirmSave')}
        description={(
          <span className="grid gap-2">
            {turningOff ? <span>{t(wifi.mode === 'ap' ? 'network.wifi.offApCopy' : 'network.wifi.offClientCopy', { radio })}</span> : null}
            {modeChanged ? <span>{t('network.wifi.switchCopy', { from: t(`network.wifi.modes.${wifi.mode}`) })}</span> : null}
            {modeChanged && draft?.mode === 'client' ? <span>{t('network.wifi.reachCopy', { host: `${window.location.protocol}//${data.hostname ?? 'mica'}.local${window.location.port ? `:${window.location.port}` : ''}` })}</span> : null}
            {openNow ? <span className="text-destructive">{t('network.wifi.openCopy')}</span> : null}
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

/// The pool micad derives when none is declared: from the address after the
/// access point's to the one before the broadcast address.
function derivedPool(address: string): { poolStart: string; poolEnd: string } {
  const subnet = parseCidr(address)
  return subnet && subnet.ip + 1 < subnet.broadcast ? { poolStart: intToIp(subnet.ip + 1), poolEnd: intToIp(subnet.broadcast - 1) } : { poolStart: '', poolEnd: '' }
}

/// The access point fields a draft writes. The device stores a pool equal to
/// the derived one as none, so it keeps following the address.
function apFields(draft: Draft) {
  return {
    ssid: draft.ssid,
    channel: draft.channel,
    countryCode: draft.country,
    address: draft.address,
    security: draft.security,
    dhcpServer: { poolStart: draft.poolStart, poolEnd: draft.poolEnd, leaseSeconds: draft.lease ? Number(draft.lease) : undefined },
  }
}

function storedFields(ap: WifiAccessPoint) {
  return {
    ssid: ap.ssid ?? '',
    channel: ap.channel,
    countryCode: ap.countryCode.toUpperCase(),
    address: ap.address,
    security: ap.security ?? 'wpa2-psk',
    dhcpServer: ap.dhcpServer ? { poolStart: ap.dhcpServer.poolStart, poolEnd: ap.dhcpServer.poolEnd, leaseSeconds: ap.dhcpServer.leaseSeconds } : undefined,
  }
}

/// `channel` as the operator reads it: 0 is the access point's own choice.
function channelLabel(channel: number, t: TFunction): string {
  return channel === 0 ? t('network.wifi.channelAuto') : t('network.wifi.channelValue', { channel })
}

/// `allowed` is what the device says the chosen country allows; until it has
/// said, the channel is not judged.
function draftErrors(draft: Draft, data: NetworkData, allowed: number[] | undefined, t: (key: string, params?: Record<string, unknown>) => string): Record<string, string> {
  const errors: Record<string, string> = {}
  if (!draft.enabled) return errors
  if (draft.mode === 'ap') {
    if (new TextEncoder().encode(draft.ssid).length > 32) errors.ssid = t('network.wifi.ssidLong')
    if (draft.security !== 'open' && draft.psk && !(draft.psk.length >= 8 && draft.psk.length <= 63)) errors.psk = t('network.wifi.pskLength')
    const subnet = parseCidr(draft.address)
    if (!subnet) errors.address = t('network.problems.cidr')
    else {
      const clash = data.applied.order.find((name) => name !== data.wifi.radio && data.applied.ifaces[name].obs?.address && overlaps(draft.address, data.applied.ifaces[name].obs?.address ?? ''))
      if (clash) errors.address = t('network.problems.overlapWith', { name: clash })
      if (!inSubnet(draft.poolStart, draft.address) || !inSubnet(draft.poolEnd, draft.address)) errors.pool = t('network.wifi.poolSubnet')
      else if (ipToInt(draft.poolStart) > ipToInt(draft.poolEnd)) errors.pool = t('network.wifi.poolOrder')
      else if (subnet.ip >= ipToInt(draft.poolStart) && subnet.ip <= ipToInt(draft.poolEnd)) errors.pool = t('network.wifi.poolOwn', { address: intToIp(subnet.ip) })
    }
    if (draft.lease && !(Number(draft.lease) >= MIN_LEASE)) errors.lease = t('network.wifi.leaseShort', { count: MIN_LEASE })
    if (!draft.countryConfirmed) errors.country = t('network.wifi.confirmCountry')
    if (allowed && !allowed.includes(draft.channel)) errors.channel = t('network.wifi.channelInvalid')
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
  const wifiText = useWifiText()
  const radio = wifi.radio
  const obs = radio ? data.applied.ifaces[radio]?.obs : undefined
  const address = obs?.address
  const association = wifi.association
  const ap = data.ap.data
  if (wifi.mode === 'off') return <FactList layout="grid" facts={[{ id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.off')}</strong> }]} />
  if (wifi.mode === 'client') {
    return <FactList layout="grid" facts={[
      { id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.modes.client')}</strong> },
      { id: 'state', label: t('network.fields.state'), value: association ? <span className="text-success">{t('network.status.connected')}</span> : <span className="text-warning">{t('network.wifi.searching')}</span> },
      { id: 'ssid', label: 'SSID', value: association?.ssid ?? '—' },
      { id: 'security', label: t('network.wifi.security'), value: association?.keyManagement ? wifiText.security(association.keyManagement) : '—' },
      { id: 'signal', label: t('network.wifi.signal'), value: association?.rssiDbm !== undefined ? wifiText.signal(association.rssiDbm) : '—' },
      { id: 'bssid', label: t('network.wifi.bssid'), value: association?.bssid ?? '—', mono: true },
      { id: 'band', label: t('network.wifi.band'), value: wifiText.bandChannel(association?.frequencyMhz) ?? '—' },
      { id: 'rate', label: t('network.wifi.linkRate'), value: association?.linkSpeedMbps ? t('network.wifi.linkRateValue', { mbps: association.linkSpeedMbps }) : '—' },
      { id: 'address', label: t('network.fields.address'), value: address ?? '—', mono: true },
      { id: 'gateway', label: t('network.fields.gateway'), value: obs?.lease?.router ?? '—', mono: true },
    ]} />
  }
  return <FactList layout="grid" facts={[
    { id: 'mode', label: t('network.wifi.mode'), value: <strong>{t('network.wifi.modes.ap')}</strong> },
    { id: 'state', label: t('network.fields.state'), value: wifi.apRunning ? <span className="text-success">{t('network.status.apRunning')}</span> : <span className="text-destructive">{t('network.status.apDown')}</span> },
    { id: 'ssid', label: 'SSID', value: <strong>{wifi.apSsid || t('network.wifi.ssidAuto')}</strong> },
    { id: 'security', label: t('network.wifi.security'), value: ap ? t(`network.wifi.apSecurities.${ap.security ?? 'wpa2-psk'}`) : '—' },
    { id: 'channel', label: t('network.wifi.channel'), value: wifi.apChannel ? wifiText.apChannel(wifi) : ap ? channelLabel(ap.channel, t) : '—' },
    { id: 'clients', label: t('network.wifi.clients'), value: t('network.wifi.clientCount', { count: wifi.stations }) },
  ]} />
}

function ConfigView({ data }: { data: NetworkData }) {
  const { t, i18n } = useTranslation()
  const wifi = data.wifi
  const ap = data.ap.data
  const entry = (wifi.radio ? data.applied.ifaces[wifi.radio]?.entry : undefined) ?? { dhcp: true }
  const radio = (
    <Group title={t('network.wifi.groups.wifi')}>
      <FactList layout="pairs" facts={[
        { id: 'enabled', label: t('network.wifi.enabled'), value: wifi.mode === 'off' ? t('network.wifi.offRadio', { radio: wifi.radio ?? '' }) : <span className="text-success">{t('network.values.on')}</span> },
        wifi.mode !== 'off' && { id: 'mode', label: t('network.wifi.mode'), value: t(`network.wifi.modes.${wifi.mode}`) },
      ]} />
    </Group>
  )
  if (wifi.mode === 'ap' && ap) {
    return (
      <div className="grid gap-5">
        {radio}
        <Group title={t('network.wifi.groups.start')}>
          <FactList layout="pairs" facts={[
            { id: 'start', label: t('network.wifi.start'), value: t(`network.wifi.starts.${ap.mode === 'provisioning' ? 'provisioning' : 'always'}`) },
          ]} />
        </Group>
        <Group title={t('network.wifi.groups.network')}>
          <FactList layout="pairs" facts={[
            { id: 'ssid', label: 'SSID', value: ap.ssid || t('network.wifi.ssidAuto') },
            { id: 'security', label: t('network.wifi.security'), value: t(`network.wifi.apSecurities.${ap.security ?? 'wpa2-psk'}`) },
            ap.security !== 'open' && { id: 'psk', label: t('network.wifi.password'), value: t('network.wifi.pskHidden') },
          ]} />
        </Group>
        <Group title={t('network.wifi.groups.radio')}>
          <FactList layout="pairs" facts={[
            { id: 'country', label: t('network.wifi.country'), value: `${countryName(ap.countryCode, i18n.resolvedLanguage ?? 'en')} (${ap.countryCode})` },
            { id: 'channel', label: t('network.wifi.channel'), value: channelLabel(ap.channel, t) },
          ]} />
        </Group>
        <Group title={t('network.wifi.groups.addressing')}>
          <FactList layout="pairs" facts={[
            { id: 'address', label: t('network.wifi.apAddress'), value: ap.address, mono: true },
            { id: 'pool', label: t('network.wifi.pool'), value: ap.dhcpServer ? `${ap.dhcpServer.poolStart} – ${ap.dhcpServer.poolEnd}` : '—', mono: true },
            { id: 'lease', label: t('network.wifi.lease'), value: ap.dhcpServer?.leaseSeconds ? t('network.values.seconds', { count: ap.dhcpServer.leaseSeconds }) : t('network.wifi.leaseDefault') },
            { id: 'dhcp', label: t('network.wifi.apDhcp'), value: t('network.wifi.apDhcpCopy') },
          ]} />
        </Group>
      </div>
    )
  }
  if (wifi.mode === 'client') {
    return (
      <div className="grid gap-5">
        {radio}
        <Group title={t('network.detail.addressAndDns')}>
          <FactList layout="pairs" facts={[
            { id: 'preset', label: t('network.detail.preset'), value: t(entry.dhcp ? 'network.presets.dhcp' : 'network.presets.static') },
            ...(entry.dhcp
              ? [{ id: 'dns', label: t('network.fields.dns'), value: entry.dns?.length ? entry.dns.join(', ') : t('network.detail.leaseDns') }]
              : [
                  { id: 'address', label: t('network.fields.address'), value: entry.static?.address || '—', mono: true },
                  { id: 'gateway', label: t('network.fields.gateway'), value: entry.static?.gateway || t('network.values.none'), mono: true },
                  { id: 'dns', label: t('network.fields.dns'), value: entry.static?.dns?.length ? entry.static.dns.join(', ') : t('network.values.none'), mono: true },
                ]),
          ]} />
        </Group>
      </div>
    )
  }
  return radio
}

function ConfigForm({ draft, setDraft, errors, current, allowed }: { draft: Draft; setDraft: (draft: Draft) => void; errors: Record<string, string>; current: 'off' | 'client' | 'ap'; allowed: number[] | undefined }) {
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
              {(id) => <Input id={id} className="sm:w-50" value={draft.ssid} aria-invalid={Boolean(errors.ssid)} onChange={(event) => setDraft({ ...draft, ssid: event.target.value })} />}
            </FormField>
            <FormField label={t('network.wifi.security')}>
              {(id) => (
                <Select value={draft.security} onValueChange={(value) => setDraft({ ...draft, security: value as ApSecurity })}>
                  <SelectTrigger id={id} className="w-50"><SelectValue>{t(`network.wifi.apSecurities.${draft.security}`)}</SelectValue></SelectTrigger>
                  <SelectContent>{AP_SECURITIES.map((each) => <SelectItem key={each} value={each}>{t(`network.wifi.apSecurities.${each}`)}</SelectItem>)}</SelectContent>
                </Select>
              )}
            </FormField>
          </div>
          {draft.security === 'open' ? <Callout tone="warning" title={t('network.wifi.openWarning')} /> : (
            <FormField label={t('network.wifi.password')} hint={hint('psk', t('network.wifi.pskHint'))}>
              {(id) => <Input id={id} className="sm:w-80" type="password" value={draft.psk} placeholder={t('network.wifi.pskKeep')} aria-invalid={Boolean(errors.psk)} onChange={(event) => setDraft({ ...draft, psk: event.target.value })} />}
            </FormField>
          )}
          <FormField label={t('network.wifi.country')} hint={hint('country', t('network.wifi.countryHint'))}>
            {(id) => (
              <div className="grid gap-2">
                <Select value={draft.country} onValueChange={(value) => { const country = String(value); setDraft({ ...draft, country }) }}>
                  <SelectTrigger id={id} className="w-80 max-w-full"><SelectValue>{`${countryName(draft.country, locale)} (${draft.country})`}</SelectValue></SelectTrigger>
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
                  <SelectTrigger id={id} className="w-50 font-mono"><SelectValue>{channelLabel(draft.channel, t)}</SelectValue></SelectTrigger>
                  <SelectContent>{(allowed ?? [draft.channel]).map((channel) => <SelectItem key={channel} value={String(channel)}>{channelLabel(channel, t)}</SelectItem>)}</SelectContent>
                </Select>
              )}
            </FormField>
            <FormField label={t('network.wifi.apAddress')} hint={hint('address', t('network.wifi.apAddressHint'))}>
              {(id) => <Input id={id} className="font-mono sm:w-50" value={draft.address} aria-invalid={Boolean(errors.address)} onChange={(event) => { const address = event.target.value.trim(); const derived = derivedPool(draft.address); const following = draft.poolStart === derived.poolStart && draft.poolEnd === derived.poolEnd; setDraft({ ...draft, address, ...(following ? derivedPool(address) : {}) }) }} />}
            </FormField>
            <FormField label={t('network.wifi.pool')} hint={hint('pool', t('network.wifi.poolHint'))}>
              {(id) => (
                <div className="flex items-center gap-2">
                  <Input id={id} className="font-mono sm:w-50" aria-label={t('network.wifi.poolStart')} value={draft.poolStart} aria-invalid={Boolean(errors.pool)} onChange={(event) => setDraft({ ...draft, poolStart: event.target.value.trim() })} />
                  <span>–</span>
                  <Input className="font-mono sm:w-50" aria-label={t('network.wifi.poolEnd')} value={draft.poolEnd} aria-invalid={Boolean(errors.pool)} onChange={(event) => setDraft({ ...draft, poolEnd: event.target.value.trim() })} />
                </div>
              )}
            </FormField>
            <FormField label={t('network.wifi.leaseSeconds')} hint={hint('lease', t('network.wifi.leaseHint'))}>
              {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={draft.lease} placeholder={t('network.wifi.leaseDefault')} aria-invalid={Boolean(errors.lease)} onChange={(event) => setDraft({ ...draft, lease: event.target.value.replace(/\D/g, '') })} />}
            </FormField>
          </div>
          <p className="text-sm text-muted-foreground">{t('network.wifi.apGatewayNote')}</p>
        </>
      ) : (
        <>
          <FormField label={t('network.detail.preset')}>
            {() => <SegmentedControl<'dhcp' | 'static'> label={t('network.detail.preset')} cards value={entry.dhcp ? 'dhcp' : 'static'} onValueChange={(preset) => setEntry(preset === 'dhcp' ? { ...withoutAddressing(entry), dhcp: true } : { ...withoutAddressing(entry), dhcp: false, static: { address: entry.static?.address ?? '', ...(entry.static?.gateway ? { gateway: entry.static.gateway } : {}), dns: entry.dns ?? entry.static?.dns ?? [] } })} segments={[{ value: 'dhcp', label: t('network.presets.dhcp') }, { value: 'static', label: t('network.presets.static') }]} />}
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
                  {(id) => <Input id={id} className="font-mono sm:w-80" defaultValue={(entry.dns ?? []).join(', ')} onChange={(event) => setEntry({ ...entry, dns: split(event.target.value) })} />}
                </FormField>
              ) : null}
            </>
          ) : (
            <div className="grid gap-4 sm:grid-cols-2">
              <FormField label={t('network.fields.address')} hint={hint('address', t('network.detail.cidrHint'))}>
                {(id) => <Input id={id} className="font-mono sm:w-50" value={entry.static?.address ?? ''} placeholder="192.168.31.20/24" onChange={(event) => setEntry({ ...entry, static: { ...entry.static, address: event.target.value.trim() } })} />}
              </FormField>
              <FormField label={t('network.fields.gateway')} hint={hint('gateway')}>
                {(id) => <Input id={id} className="font-mono sm:w-50" value={entry.static?.gateway ?? ''} placeholder={t('network.values.none')} onChange={(event) => { const gateway = event.target.value.trim(); const next = { ...entry, static: { address: entry.static?.address ?? '', ...entry.static } }; if (gateway) next.static.gateway = gateway; else delete next.static.gateway; setEntry(next) }} />}
              </FormField>
              <FormField className="sm:col-span-2" label={t('network.fields.dns')} hint={hint('dns', t('network.detail.listHint'))}>
                {(id) => <Input id={id} className="font-mono sm:w-80" defaultValue={(entry.static?.dns ?? []).join(', ')} onChange={(event) => setEntry({ ...entry, static: { address: entry.static?.address ?? '', ...entry.static, dns: split(event.target.value) } })} />}
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
  return next
}

/// The client's saved networks and the radio's scan.
function ClientLists({ data, locked }: { data: NetworkData; locked: boolean }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const networks = useQuery({ queryKey: ['wifi-networks'], queryFn: () => api<WifiNetwork[]>('/api/v1/wifi/client/networks') })
  const [dialog, setDialog] = useState<{ ssid: string; existing: boolean; secured: boolean }>()
  const [psk, setPsk] = useState('')
  const [hidden, setHidden] = useState(false)
  const [priority, setPriority] = useState('0')
  const [autoConnect, setAutoConnect] = useState(true)
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
    setDialog(next); setSsid(next.ssid); setPsk(''); setHidden(entry?.hidden ?? false); setPriority(String(entry?.priority ?? 0)); setAutoConnect(entry?.autoConnect !== false)
  }
  const pskError = psk && !(psk.length >= 8 && psk.length <= 63) ? t('network.wifi.pskLength') : undefined
  const ssidError = !dialog?.existing && ssid && networks.data?.some((each) => each.ssid === ssid) ? t('network.wifi.ssidSaved') : undefined
  const submit = () => {
    if (!dialog) return Promise.resolve()
    // Absent is joined automatically, which a replacing PUT then restores.
    const body = { ssid, ...(psk ? { psk } : {}), hidden, priority: Number(priority), ...(autoConnect ? {} : { autoConnect: false }) }
    const request = dialog.existing
      ? api<WifiNetwork>(`/api/v1/wifi/client/networks/${encodeURIComponent(dialog.ssid)}`, json('PUT', body))
      : api<WifiNetwork>('/api/v1/wifi/client/networks', json('POST', body))
    return request.then(() => queryClient.invalidateQueries({ queryKey: ['wifi-networks'] }))
  }
  const wifiText = useWifiText()
  const signal = (network: ScannedNetwork) => network.best.signalDbm === undefined ? '—' : wifiText.signal(network.best.signalDbm)

  return (
    <>
      <Section
        title={t('network.wifi.savedNetworks')}
        description={t('network.wifi.savedCopy')}
        action={<Button size="sm" variant="outline" disabled={locked} onClick={() => open({ ssid: '', existing: false, secured: true })}><Plus />{t('network.wifi.add')}</Button>}
        flush
      >
        {networks.error ? <div className="p-4"><Callout tone="danger" title={failureDetail(networks.error, t('common.requestFailed'))} /></div> : null}
        <DataTable<WifiNetwork>
          rows={networks.data ? [...networks.data].sort((left, right) => right.priority - left.priority) : undefined}
          rowKey={(entry) => entry.ssid}
          isPending={networks.isPending}
          empty={t('network.wifi.empty')}
          columns={[
            { id: 'ssid', header: t('network.wifi.network'), cell: (entry) => <span className="flex flex-wrap items-center gap-1.5"><strong>{entry.ssid}</strong>{entry.hidden ? <span className="text-xs text-muted-foreground">{t('network.wifi.hidden')}</span> : null}{wifi.association?.ssid === entry.ssid ? <span className="text-xs text-primary">{t('network.status.connected')}</span> : null}</span> },
            { id: 'security', header: t('network.wifi.security'), cell: (entry) => t(entry.psk ? 'network.wifi.wpa' : 'network.wifi.open') },
            { id: 'priority', header: t('network.wifi.priority'), cell: (entry) => entry.priority },
            { id: 'auto', header: t('network.wifi.autoConnect'), cell: (entry) => t(entry.autoConnect === false ? 'network.values.off' : 'network.values.on') },
            { id: 'actions', header: '', align: 'end', cell: (entry) => (
              <span className="flex justify-end gap-2">
                <Button type="button" size="sm" variant="outline" disabled={locked} onClick={() => open({ ssid: entry.ssid, existing: true, secured: Boolean(entry.psk) }, entry)}>{t('common.actions.edit')}</Button>
                <ConfirmDialog
                  trigger={<Button type="button" size="sm" variant="destructive" disabled={locked} aria-label={t('network.wifi.removeNamed', { name: entry.ssid })}><Trash2 /></Button>}
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
      </Section>

      <Section
        title={t('network.wifi.nearby')}
        description={t('network.wifi.nearbyCopy')}
        action={<Button size="sm" variant="outline" disabled={Boolean(scanBlocked) || scan.isPending} onClick={() => scan.mutate()}><Radar />{scan.isPending ? t('network.wifi.scanning') : t('network.wifi.scan')}</Button>}
        flush
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
                { id: 'band', header: t('network.wifi.band'), cell: (network) => [...new Set(network.accessPoints.map((bss) => wifiText.bandChannel(bss.frequencyMhz)).filter(Boolean))].join(' / ') || '—' },
                { id: 'actions', header: '', align: 'end', cell: (network) => {
                  const security = scanSecurity(network.best.flags)
                  const saved = networks.data?.some((each) => each.ssid === network.ssid)
                  return saved || security === 'eap' || security === 'wep' ? null : <Button type="button" size="sm" variant="outline" disabled={locked} onClick={() => open({ ssid: network.ssid, existing: false, secured: security !== 'open' })}>{t('network.wifi.connect')}</Button>
                } },
              ]}
            />
          </>
        )}
      </Section>

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
        <ToggleField title={t('network.wifi.autoConnect')} description={t('network.wifi.autoConnectHint')} control={<Switch checked={autoConnect} onCheckedChange={setAutoConnect} aria-label={t('network.wifi.autoConnect')} />} />
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
    <Section title={t('network.wifi.stations')} description={t('network.wifi.stationsCopy')} flush>
      <DataTable
        rows={stations}
        rowKey={(station) => station.mac}
        empty={t('network.wifi.noStations')}
        columns={[
          { id: 'host', header: t('network.wifi.host'), cell: (station) => station.hostname || station.address ? <span>{station.hostname ?? '—'} <span className="font-mono text-[0.8125rem] text-muted-foreground">{station.address ?? ''}</span></span> : '—' },
          { id: 'mac', header: 'MAC', cell: (station) => <span className="font-mono text-[0.8125rem]">{station.mac}</span> },
          { id: 'signal', header: t('network.wifi.signal'), cell: (station) => station.signalDbm !== undefined ? `${station.signalDbm} dBm` : '—' },
          { id: 'connected', header: t('network.wifi.connectedFor'), cell: (station) => station.connectedSeconds !== undefined ? t('network.wifi.minutes', { count: Math.floor(station.connectedSeconds / 60) }) : '—' },
          { id: 'traffic', header: t('network.wifi.traffic'), cell: (station) => <span className={cn('text-muted-foreground')}>{bytes(station.rxBytes)} / {bytes(station.txBytes)}</span> },
        ]}
      />
    </Section>
  )
}
