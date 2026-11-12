import { useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Clock3, Globe2, Satellite } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, TimeStatus, TimeZoneCatalog } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { FactList } from '@/shared/components/fact-list'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { StatusDot } from '@/shared/components/status-badge'
import { Combobox, ComboboxCollection, ComboboxContent, ComboboxEmpty, ComboboxGroup, ComboboxInput, ComboboxItem, ComboboxLabel, ComboboxList } from '@/shared/components/ui/combobox'
import { Textarea } from '@/shared/components/ui/textarea'
import { TaskProgress } from '@/shared/components/task-progress'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { failureDetail } from '@/shared/feedback/toast'

export function TimePanel() {
  return (
    <div className="grid gap-3">
      <div className="grid gap-3 lg:grid-cols-2"><NtpServersPanel /><TimezonePanel /></div>
      <SyncStatusPanel />
    </div>
  )
}

export function NtpServersPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const servers = useQuery({ queryKey: ['settings', 'time.ntp.servers'], queryFn: () => api<string[]>('/api/v1/settings/time.ntp.servers') })
  const [draft, setDraft] = useState<string>()
  const update = useMutationFeedback<TaskAccepted, string[]>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings/time.ntp.servers', json('PUT', value)),
    success: t('system.time.servers.saved'),
    failure: t('system.time.servers.save'),
    // The draft is dropped so the field falls back to what the device confirms,
    // rather than keeping showing the string this browser sent.
    onSuccess: () => { setDraft(undefined); void queryClient.invalidateQueries({ queryKey: ['settings', 'time.ntp.servers'] }) },
  })
  const value = draft ?? (servers.data ?? []).join('\n')
  const parsed = value.split('\n').map((line) => line.trim()).filter(Boolean)
  return (
    <Panel title={t('system.time.servers.title')} description={t('system.time.servers.description')} action={<Satellite className="size-5 text-muted-foreground" />}>
      <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); update.mutate(parsed) }}>
        <FormField label={t('system.time.servers.label')} hint={t('system.time.servers.hint')}>
          {(id) => (
            <Textarea
              id={id}
              className="min-h-28 font-mono"
              value={value}
              onChange={(event) => setDraft(event.target.value)}
              placeholder={'0.pool.ntp.org\ntime.example.com'}
            />
          )}
        </FormField>
        <Button className="justify-self-end" type="submit" disabled={update.isPending || draft === undefined}>{t('system.time.servers.save')}</Button>
        <TaskProgress taskId={update.data?.taskId} />
        {servers.error ? <Callout tone="danger" title={failureDetail(servers.error, t('common.requestFailed'))} /> : null}
      </form>
    </Panel>
  )
}

export function TimezonePanel() {
  const { t, i18n } = useTranslation()
  const queryClient = useQueryClient()
  const timezone = useQuery({ queryKey: ['settings', 'time.timezone'], queryFn: () => api<string>('/api/v1/settings/time.timezone') })
  const catalog = useQuery({ queryKey: ['time-zones'], queryFn: () => api<TimeZoneCatalog>('/api/v1/time/zones'), retry: false })
  const [draft, setDraft] = useState<string>()
  const update = useMutationFeedback<TaskAccepted, string>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings/time.timezone', json('PUT', value)),
    success: t('system.time.timezone.saved'),
    failure: t('system.time.timezone.save'),
    onSuccess: () => {
      setDraft(undefined)
      void queryClient.invalidateQueries({ queryKey: ['settings', 'time.timezone'] })
      void queryClient.invalidateQueries({ queryKey: ['time-zones'] })
    },
  })
  const groups = useMemo(() => zoneGroups(catalog.data?.zones ?? [], t), [catalog.data, t, i18n.resolvedLanguage])
  const stored = timezone.data
  const chosen = draft ?? stored
  const selected = groups.flatMap((group) => group.items).find((option) => option.value === chosen) ?? null
  const current = catalog.data?.current
  return (
    <Panel title={t('system.time.timezone.title')} description={t('system.time.timezone.description')} action={<Globe2 className="size-5 text-muted-foreground" />}>
      <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); if (draft) update.mutate(draft) }}>
        {catalog.data ? (
          <FormField label={t('system.time.timezone.label')} hint={t('system.time.timezone.hint')}>
            {(id) => (
              <Combobox
                items={groups}
                value={selected}
                onValueChange={(option: ZoneOption | null) => setDraft(option ? option.value : undefined)}
                isItemEqualToValue={(a: ZoneOption, b: ZoneOption) => a.value === b.value}
              >
                <ComboboxInput id={id} className="w-full" placeholder={t('system.time.timezone.search')} />
                <ComboboxContent>
                  <ComboboxEmpty>{t('system.time.timezone.none')}</ComboboxEmpty>
                  <ComboboxList>
                    {(group: ZoneGroup) => (
                      <ComboboxGroup key={group.value} items={group.items}>
                        {group.value ? <ComboboxLabel>{group.value}</ComboboxLabel> : null}
                        <ComboboxCollection>
                          {(option: ZoneOption) => <ComboboxItem key={option.value} value={option}>{option.label}</ComboboxItem>}
                        </ComboboxCollection>
                      </ComboboxGroup>
                    )}
                  </ComboboxList>
                </ComboboxContent>
              </Combobox>
            )}
          </FormField>
        ) : stored ? (
          <FactList facts={[{ id: 'stored', label: t('system.time.timezone.label'), value: stored }]} />
        ) : null}
        {current && !current.installed ? <Callout tone="warning" title={t('system.time.timezone.notInstalled', { id: current.id })} /> : null}
        {current && current.installed && !current.listed ? <Callout title={t('system.time.timezone.compatible', { id: current.id })} /> : null}
        {catalog.error ? (
          <Callout tone="danger" title={failureDetail(catalog.error, t('common.requestFailed'))}>
            <Button type="button" variant="outline" size="sm" onClick={() => void catalog.refetch()} disabled={catalog.isFetching}>{t('system.time.timezone.retry')}</Button>
          </Callout>
        ) : null}
        <Button className="justify-self-end" type="submit" disabled={update.isPending || draft === undefined || draft === stored}>{t('system.time.timezone.save')}</Button>
        <TaskProgress taskId={update.data?.taskId} />
        {timezone.error ? <Callout tone="danger" title={failureDetail(timezone.error, t('common.requestFailed'))} /> : null}
      </form>
    </Panel>
  )
}

type ZoneOption = { value: string; label: string }
type ZoneGroup = { value: string; items: ZoneOption[] }

/// The catalog as picker groups: `UTC` alone first, then one group per region
/// in catalog order. A label reads `Region / City · IANA ID · UTC±hh:mm`, the
/// offset being the one the zone has now, so a search matches any of them.
function zoneGroups(zones: TimeZoneCatalog['zones'], t: ReturnType<typeof useTranslation>['t']): ZoneGroup[] {
  const now = new Date()
  const groups: ZoneGroup[] = []
  for (const zone of zones) {
    const region = zone.region ? t(`system.time.timezone.regions.${zone.region}`, { defaultValue: zone.region }) : ''
    const offset = utcOffset(zone.id, now)
    const label = zone.region && zone.city
      ? `${[region, ...zone.city.split('/')].join(' / ').replaceAll('_', ' ')} · ${zone.id}${offset ? ` · ${offset}` : ''}`
      : `${zone.id}${offset ? ` · ${offset}` : ''}`
    const last = groups.at(-1)
    if (last && last.value === region) last.items.push({ value: zone.id, label })
    else groups.push({ value: region, items: [{ value: zone.id, label }] })
  }
  return groups
}

/// `UTC+01:00` for a zone at `at`, from the browser's own tzdata; empty when
/// the browser does not know the zone.
function utcOffset(zone: string, at: Date) {
  try {
    const name = new Intl.DateTimeFormat('en-US', { timeZone: zone, timeZoneName: 'longOffset' })
      .formatToParts(at)
      .find((part) => part.type === 'timeZoneName')?.value
    if (!name) return ''
    return name === 'GMT' ? 'UTC+00:00' : name.replace('GMT', 'UTC')
  } catch {
    return ''
  }
}

export function SyncStatusPanel() {
  const { t } = useTranslation()
  const status = useQuery({
    queryKey: ['time-status'],
    queryFn: () => api<TimeStatus>('/api/v1/time/status'),
    refetchInterval: 15_000,
    retry: false,
  })
  const value = status.data
  return (
    <Panel title={t('system.time.status.title')} description={t('system.time.status.description')} action={<Clock3 className="size-5 text-muted-foreground" />}>
      <StatusDot state={status.isPending ? 'pending' : value?.status === 'synchronized' ? 'ok' : 'warning'}>
        {status.isPending ? t('system.time.status.checking') : t(`system.time.status.states.${value?.status ?? 'unknown'}`)}
      </StatusDot>
      <FactList facts={[
        value?.server ? { id: 'server', label: t('system.time.status.server'), value: serverSummary(value.server, t) } : undefined,
        value?.sample ? { id: 'sample', label: t('system.time.status.sample'), value: t('system.time.status.sampleValue', { stratum: value.sample.stratum, offset: formatOffset(value.sample.offsetSeconds) }) } : undefined,
        value?.sample ? { id: 'correction', label: t('system.time.status.correction'), value: t(value.sample.correction === 'step' ? 'system.time.status.stepped' : 'system.time.status.slewing') } : undefined,
      ]} />
      {value?.detail ? <Callout tone="warning" title={value.detail} /> : null}
      {status.error ? <Callout tone="danger" title={failureDetail(status.error, t('common.requestFailed'))} /> : null}
    </Panel>
  )
}

function serverSummary(server: NonNullable<TimeStatus['server']>, t: ReturnType<typeof useTranslation>['t']) {
  if (server.name && server.address) return `${server.name} (${server.address})`
  return server.name ?? server.address ?? t('common.notAvailable')
}

function formatOffset(seconds: number) {
  const abs = Math.abs(seconds)
  if (abs < 1) return `${(seconds * 1000).toFixed(1)} ms`
  return `${seconds.toFixed(3)} s`
}
