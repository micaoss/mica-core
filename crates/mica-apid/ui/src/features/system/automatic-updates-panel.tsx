import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { CalendarClock, Timer } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { FactList } from '@/shared/components/fact-list'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

/// The **resolved** policy, as `GET /api/v1/update` reports it under
/// `lifecycle.policy`: the precedence already applied, which is
/// what the device is actually following. Which layer each value came from is
/// the provisioning-status route's answer and this panel never re-derives it.
interface ResolvedPolicy {
  policy?: string | null
  checkIntervalMinutes?: number | null
  checkAt?: string | null
  sourceUrl?: string | null
  channel?: string | null
  rebootPolicy?: string
  maintenanceWindows?: MaintenanceWindow[]
}

interface MaintenanceWindow {
  days?: string[]
  start?: string
  end?: string
}

interface Deferral {
  reason?: string
  detail?: string
  waitedSeconds?: number
  attempts?: number
}

interface UpdatePolicyDoc {
  lifecycle?: {
    policy?: ResolvedPolicy
    policy_error?: string
    deferred?: Deferral
  }
}

/// The three layers of `GET /api/v1/provisioning/status`, for
/// the source address as well as the channel.
interface ProvisioningLayers {
  baked?: { update?: { source?: string | null; channel?: string; policy?: string } }
  operator?: { update?: { source?: string | null; channel?: string | null; policy?: string | null } }
  effective?: { update?: { source?: string | null; channel?: string; policy?: string } }
}

/// The patch `POST /api/v1/update/config` takes: only the keys being changed.
interface UpdateConfigPatch {
  policy?: string
  checkIntervalMinutes?: number
  /// `HH:MM` UTC, or `null` to go back to the interval.
  checkAt?: string | null
  rebootPolicy?: string
  source?: { url?: string | null; channel?: string | null }
  maintenance?: { windows: { days: string[]; start: string; end: string }[] }
}

const POLICIES = ['off', 'check', 'auto'] as const
const REBOOT_POLICIES = ['manual', 'window'] as const

/// The cadence the document holds is a minute count. An operator thinks in
/// days or hours, so the field is a number and a unit, and the largest unit
/// that divides the stored value exactly is the one it is shown in: 1440
/// reads as one day, 90 stays ninety minutes.
const CADENCE_UNITS = { minutes: 1, hours: 60, days: 1440 } as const

type CadenceUnit = keyof typeof CADENCE_UNITS

function cadenceOf(minutes: number): { value: number; unit: CadenceUnit } {
  if (minutes > 0 && minutes % CADENCE_UNITS.days === 0) return { value: minutes / CADENCE_UNITS.days, unit: 'days' }
  if (minutes > 0 && minutes % CADENCE_UNITS.hours === 0) return { value: minutes / CADENCE_UNITS.hours, unit: 'hours' }
  return { value: minutes, unit: 'minutes' }
}

/// One write, three panes, three sentences.
///
/// The three forms shared a single mutation and reported nothing at all on
/// success, so saving a policy and losing the click looked the same. They still
/// share the endpoint and the two invalidations; what differs is what the
/// operator is told, which is per-form and therefore per-hook.
function useWriteConfig(success: string, failure: string) {
  const queryClient = useQueryClient()
  return useMutationFeedback<unknown, UpdateConfigPatch>({
    mutationFn: (patch) => api<unknown>('/api/v1/update/config', json('POST', patch)),
    success,
    failure,
    onSuccess: () => {
      // Both reads move: the resolved policy, and which layer it came from.
      void queryClient.invalidateQueries({ queryKey: ['update-state'] })
      void queryClient.invalidateQueries({ queryKey: ['provisioning-status'] })
    },
  })
}

export function AutomaticUpdatesPanel() {
  const { t } = useTranslation()
  // The same query key the rest of the update tab reads: one document, one
  // fetch, and no second answer about the same policy.
  const state = useQuery({ queryKey: ['update-state'], queryFn: () => api<UpdatePolicyDoc>('/api/v1/update') })
  const layers = useQuery({ queryKey: ['provisioning-status'], queryFn: () => api<ProvisioningLayers>('/api/v1/provisioning/status') })
  const policy = state.data?.lifecycle?.policy
  return (
    <div className="grid gap-3">
      <SourcePanel layers={layers.data} error={layers.error} />
      <PolicyPanel policy={policy} />
      <WindowsPanel windows={policy?.maintenanceWindows} />
      <DeferralNotice deferred={state.data?.lifecycle?.deferred} />
      {state.data?.lifecycle?.policy_error ? <Callout tone="danger" title={t('system.update.automatic.documentError', { reason: state.data.lifecycle.policy_error })} /> : null}
      {state.error ? <Callout tone="danger" title={failureDetail(state.error, t('common.requestFailed'))} /> : null}
    </div>
  )
}

/// Where this device looks for updates: the address and the channel, each read
/// as baked / operator / effective, and each writable.
///
/// The two fields hold the **operator's** value, not the effective one, and an
/// empty field is `null`: clearing the box returns the device to the image's
/// default rather than writing that default back as an override.
function SourcePanel({ layers, error }: { layers?: ProvisioningLayers; error: unknown }) {
  const { t } = useTranslation()
  const write = useWriteConfig(t('system.update.automatic.sourceSaved'), t('system.update.automatic.saveSource'))
  const baked = layers?.baked?.update
  const operator = layers?.operator?.update
  const effective = layers?.effective?.update
  const [url, setUrl] = useState<string>()
  const [channel, setChannel] = useState<string>()
  const currentUrl = url ?? operator?.source ?? ''
  const currentChannel = channel ?? operator?.channel ?? ''
  const rows = [
    {
      id: 'source',
      label: t('system.update.automatic.source'),
      effective: effective?.source ?? undefined,
      baked: baked?.source ?? undefined,
      overridden: operator !== undefined && 'source' in operator,
    },
    {
      id: 'channel',
      label: t('system.update.automatic.channel'),
      effective: effective?.channel,
      baked: baked?.channel,
      overridden: operator !== undefined && 'channel' in operator,
    },
  ]
  return (
    <Panel title={t('system.update.automatic.sourceTitle')} description={t('system.update.automatic.sourceDescription')} action={<CalendarClock className="size-5 text-muted-foreground" />}>
      <FactList facts={rows.map((row) => ({
        id: row.id,
        label: row.label,
        value: (
          <span className="flex flex-col items-end gap-0.5">
            <span className="font-mono">{row.effective ?? t('system.update.automatic.noSource')}</span>
            <span className="text-xs text-muted-foreground">
              {row.overridden
                ? t('system.update.automatic.overridden', { baked: row.baked ?? t('system.update.automatic.noSource') })
                : t('system.update.automatic.fromImage')}
            </span>
          </span>
        ),
      }))} />
      <form
        className="grid gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault()
          write.mutate({ source: { url: currentUrl.trim() || null, channel: currentChannel.trim() || null } })
        }}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('system.update.automatic.urlLabel')} hint={t('system.update.automatic.urlHint')}>
            {/* No invented fallback address: an image that bakes no source has no server. */}
            {(id) => <Input id={id} value={currentUrl} onChange={(event) => setUrl(event.target.value)} placeholder={baked?.source ?? undefined} />}
          </FormField>
          <FormField label={t('system.update.automatic.channelLabel')} hint={t('system.update.automatic.channelHint')}>
            {(id) => <Input id={id} value={currentChannel} onChange={(event) => setChannel(event.target.value)} placeholder={baked?.channel ?? 'stable'} />}
          </FormField>
        </div>
        <Button className="justify-self-end" type="submit" disabled={write.isPending}>
          {write.isPending ? <Spinner /> : null}{t('system.update.automatic.saveSource')}
        </Button>
      </form>
      {error ? <Callout tone="danger" title={failureDetail(error, t('common.requestFailed'))} /> : null}
    </Panel>
  )
}

/// What the device does on its own: the mode, the cadence and what happens
/// after an automatic install.
///
/// These three are written explicitly, unlike the address and channel above:
/// choosing a mode in a selector IS the operator deciding, so recording it in
/// their layer is what they meant.
function PolicyPanel({ policy }: { policy?: ResolvedPolicy }) {
  const { t } = useTranslation()
  const write = useWriteConfig(t('system.update.automatic.policySaved'), t('system.update.automatic.save'))
  const [mode, setMode] = useState<string>()
  const [interval, setInterval] = useState<string>()
  const [unit, setUnit] = useState<CadenceUnit>()
  const [rebootPolicy, setRebootPolicy] = useState<string>()
  const [checkAt, setCheckAt] = useState<string>()
  const stored = cadenceOf(policy?.checkIntervalMinutes ?? 1440)
  const currentMode = mode ?? policy?.policy ?? 'check'
  const currentInterval = interval ?? stored.value.toString()
  const currentUnit = unit ?? stored.unit
  const currentReboot = rebootPolicy ?? policy?.rebootPolicy ?? 'manual'
  const currentCheckAt = checkAt ?? policy?.checkAt ?? ''
  const knownMode = POLICIES.find((known) => known === currentMode)
  const knownReboot = REBOOT_POLICIES.find((known) => known === currentReboot)
  return (
    <Panel title={t('system.update.automatic.policyTitle')} description={t('system.update.automatic.policyDescription')} action={<Timer className="size-5 text-muted-foreground" />}>
      <form
        className="grid gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault()
          write.mutate({
            policy: currentMode,
            checkIntervalMinutes: Number(currentInterval) * CADENCE_UNITS[currentUnit],
            // An empty box is `null`, which is the device back on its
            // interval -- sending an empty string would store one.
            checkAt: currentCheckAt.trim() ? currentCheckAt.trim() : null,
            rebootPolicy: currentReboot,
          })
        }}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('system.update.automatic.mode')} hint={knownMode ? t(`system.update.automatic.modes.${knownMode}`) : undefined}>
            {(id) => (
              <Select value={currentMode} onValueChange={(value) => setMode(String(value))}>
                <SelectTrigger id={id} aria-label={t('system.update.automatic.mode')}><SelectValue /></SelectTrigger>
                <SelectContent>{POLICIES.map((option) => <SelectItem value={option} key={option}>{t(`system.update.automatic.modeNames.${option}`)}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
          <FormField label={t('system.update.automatic.interval')} hint={t('system.update.automatic.intervalHint')}>
            {(id) => (
              <div className="flex gap-2">
                <Input id={id} type="number" min={0} value={currentInterval} onChange={(event) => setInterval(event.target.value)} required />
                <Select value={currentUnit} onValueChange={(value) => setUnit(String(value) as CadenceUnit)}>
                  <SelectTrigger className="w-32" aria-label={t('system.update.automatic.intervalUnit')}><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {(Object.keys(CADENCE_UNITS) as CadenceUnit[]).map((option) => (
                      <SelectItem value={option} key={option}>{t(`system.update.automatic.intervalUnits.${option}`)}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
          </FormField>
          <FormField label={t('system.update.automatic.checkAt')} hint={t('system.update.automatic.checkAtHint')}>
            {(id) => <Input id={id} type="time" value={currentCheckAt} onChange={(event) => setCheckAt(event.target.value)} />}
          </FormField>
          <FormField label={t('system.update.automatic.rebootPolicy')} hint={knownReboot ? t(`system.update.automatic.rebootPolicies.${knownReboot}`) : undefined}>
            {(id) => (
              <Select value={currentReboot} onValueChange={(value) => setRebootPolicy(String(value))}>
                <SelectTrigger id={id} aria-label={t('system.update.automatic.rebootPolicy')}><SelectValue /></SelectTrigger>
                <SelectContent>{REBOOT_POLICIES.map((option) => <SelectItem value={option} key={option}>{t(`system.update.automatic.rebootPolicyNames.${option}`)}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
        </div>
        <Button className="justify-self-end" type="submit" disabled={write.isPending}>
          {write.isPending ? <Spinner /> : null}{t('system.update.automatic.save')}
        </Button>
      </form>
    </Panel>
  )
}

/// The maintenance windows, edited as the list the document holds.
function WindowsPanel({ windows }: { windows?: MaintenanceWindow[] }) {
  const { t } = useTranslation()
  const write = useWriteConfig(t('system.update.automatic.windowsSaved'), t('system.update.automatic.saveWindows'))
  const [draft, setDraft] = useState<{ days: string; start: string; end: string }[]>()
  const rows = draft ?? (windows ?? []).map((window) => ({
    days: (window.days ?? []).join(', '),
    start: window.start ?? '',
    end: window.end ?? '',
  }))
  const change = (index: number, key: 'days' | 'start' | 'end', value: string) =>
    setDraft(rows.map((row, at) => at === index ? { ...row, [key]: value } : row))
  return (
    <Panel title={t('system.update.automatic.windowsTitle')} description={t('system.update.automatic.windowsDescription')}>
      <form
        className="grid gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault()
          write.mutate({
            maintenance: {
              windows: rows.map((row) => ({
                days: row.days.split(',').map((day) => day.trim().toLowerCase()).filter(Boolean),
                start: row.start.trim(),
                end: row.end.trim(),
              })),
            },
          })
        }}
      >
        {rows.length === 0 ? <p className="py-4 text-center text-sm text-muted-foreground">{t('system.update.automatic.noWindows')}</p> : null}
        {rows.map((row, index) => (
          <div className="grid items-end gap-3 sm:grid-cols-[1fr_auto_auto_auto]" key={index}>
            <FormField label={t('system.update.automatic.windowDays')} hint={t('system.update.automatic.windowDaysHint')}>
              {(id) => <Input id={id} value={row.days} onChange={(event) => change(index, 'days', event.target.value)} placeholder="mon, thu" />}
            </FormField>
            <FormField label={t('system.update.automatic.windowStart')}>
              {(id) => <Input id={id} className="sm:w-28" value={row.start} onChange={(event) => change(index, 'start', event.target.value)} placeholder="02:00" required />}
            </FormField>
            <FormField label={t('system.update.automatic.windowEnd')}>
              {(id) => <Input id={id} className="sm:w-28" value={row.end} onChange={(event) => change(index, 'end', event.target.value)} placeholder="04:00" required />}
            </FormField>
            <Button type="button" variant="outline" onClick={() => setDraft(rows.filter((_, at) => at !== index))}>{t('system.update.automatic.removeWindow')}</Button>
          </div>
        ))}
        <div className="flex flex-wrap justify-end gap-3">
          <Button type="button" variant="secondary" onClick={() => setDraft([...rows, { days: '', start: '02:00', end: '04:00' }])}>{t('system.update.automatic.addWindow')}</Button>
          <Button type="submit" disabled={write.isPending}>
            {write.isPending ? <Spinner /> : null}{t('system.update.automatic.saveWindows')}
          </Button>
        </div>
      </form>
    </Panel>
  )
}

/// What the automatic path refused, and for how long.
function DeferralNotice({ deferred }: { deferred?: Deferral }) {
  const { t } = useTranslation()
  if (!deferred?.reason) return null
  // A reason outside the vocabulary is rendered as the device sent it rather
  // than as one of these, so a refusal a newer daemon adds reads as itself in
  // an older console instead of as the wrong sentence.
  const known = DEFERRAL_REASONS.find((reason) => reason === deferred.reason)
  return (
    <Callout tone="warning" title={t('system.update.automatic.deferred', {
      reason: known ? t(`system.update.automatic.deferrals.${known}`) : deferred.reason,
    })}>
      <span className="grid gap-1">
        {deferred.detail ? <span>{deferred.detail}</span> : null}
        <span>
          {t('system.update.automatic.deferredSince', {
            minutes: Math.round((deferred.waitedSeconds ?? 0) / 60),
            attempts: deferred.attempts ?? 1,
          })}
        </span>
      </span>
    </Callout>
  )
}

/// The vocabulary `update_auto.rs` records, plus the `unknown` the daemon
/// reports a reason outside it as. The daemon clamps to this
/// set, so anything else reaching the branch below is a device older than
/// that gate.
const DEFERRAL_REASONS = [
  'check-refused',
  'clock-untrusted',
  'fetch-refused',
  'install-refused',
  'no-newer-release',
  'outside-window',
  'reboot-gate-closed',
  'reboot-pending',
  'recheck-failed',
  'recheck-refused',
  'deployment-status-unknown',
  'superseded',
  'unknown',
  'workspace-unready',
] as const
