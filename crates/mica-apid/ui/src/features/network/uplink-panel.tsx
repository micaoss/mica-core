import { useTranslation } from 'react-i18next'
import { Plus, X } from 'lucide-react'
import { formatAge } from '@/i18n/format'
import { FactList } from '@/shared/components/fact-list'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import type { UplinkObservation } from '@/lib/types'
import type { IfaceEntry, ProbeConfig, ProbeTarget } from './model'
import { Group, Section, Tag } from './parts'
import type { NetworkData } from './use-network'
import { MAX_TARGETS, megabytesPerDay, resolved, STATE_TONE, switchProbe, uplinkOf } from './uplink'
import type { Problem } from './validate'

/// Whether the uplink reaches the internet, as its check last said.
export function InternetBadge({ uplink }: { uplink: UplinkObservation }) {
  const { t } = useTranslation()
  return <StatusBadge tone={STATE_TONE[uplink.state]}>{t(`network.uplink.states.${uplink.state}`)}</StatusBadge>
}

const targetText = (target: { address: string; method: string; port?: number }) =>
  target.method === 'tcp' ? `${target.address} tcp/${target.port ?? 443}` : `${target.address} icmp`

/// The check as the device runs it: the verdict, whether this uplink
/// carries the traffic, and the last round's answers.
export function UplinkLive({ data, name }: { data: NetworkData; name: string }) {
  const { t } = useTranslation()
  const { uplink, active, failover } = uplinkOf(data.status.data, name)
  if (!uplink || uplink.state === 'off') return null
  const natural = data.status.data?.uplinks?.natural
  const round = uplink.lastRound
  return (
    <Section title={t('network.uplink.title')}>
      <div className="flex flex-wrap items-center gap-2">
        <InternetBadge uplink={uplink} />
        {active ? <Tag tone="exit">{t('network.uplink.carries')}</Tag> : null}
        {uplink.since ? <span className="text-sm text-muted-foreground">{t('network.uplink.since', { age: formatAge(Date.now() - Date.parse(uplink.since), t) })}</span> : null}
      </div>
      {uplink.state === 'blocked' ? <p className="text-sm text-warning">{t('network.uplink.blockedHint')}</p> : null}
      {failover && active && natural ? <p className="text-sm text-muted-foreground">{t('network.uplink.failover', { active: name, natural })}</p> : null}
      {round ? (
        <Group title={t('network.uplink.lastRound', { age: formatAge(Date.now() - Date.parse(round.at), t) })}>
          <FactList layout="pairs" facts={round.targets.map((result) => ({
            id: targetText(result),
            label: targetText(result),
            value: result.ok
              ? <span className="text-success">{t('network.uplink.answered', { ms: result.latencyMs ?? 0 })}</span>
              : <span className="text-destructive">{result.detail ?? t('network.uplink.noAnswer')}</span>,
          }))} />
        </Group>
      ) : null}
    </Section>
  )
}

/// The check as the entry stores it.
export function ProbeView({ entry }: { entry: IfaceEntry }) {
  const { t } = useTranslation()
  const probe = resolved(entry.probe)
  return (
    <Group title={t('network.uplink.title')}>
      {probe.enabled ? (
        <FactList layout="pairs" facts={[
          { id: 'targets', label: t('network.uplink.targets'), value: probe.targets.map(targetText).join(', '), mono: true },
          { id: 'interval', label: t('network.uplink.interval'), value: t('network.uplink.everySeconds', { seconds: probe.intervalS }) },
        ]} />
      ) : <p className="text-sm text-muted-foreground">{t('network.uplink.off')}</p>}
    </Group>
  )
}

const digits = (text: string) => text.replace(/\D/g, '')

/// The check's editor. Switched on, it fills in the two default targets,
/// which the operator may change, and says that the device will contact
/// them.
export function ProbeForm({ entry, problem, onChange }: { entry: IfaceEntry; problem?: Problem; onChange: (entry: IfaceEntry) => void }) {
  const { t } = useTranslation()
  const probe = resolved(entry.probe)
  const targets = entry.probe?.targets ?? []
  const setProbe = (change: (probe: ProbeConfig) => ProbeConfig) => onChange({ ...entry, probe: change({ enabled: probe.enabled, ...entry.probe }) })
  const setTargets = (next: ProbeTarget[]) => setProbe((current) => ({ ...current, targets: next }))
  const setTarget = (index: number, change: Partial<ProbeTarget>) => setTargets(targets.map((target, at) => {
    if (at !== index) return target
    const next = { ...target, ...change }
    if (next.method === 'icmp') delete next.port
    else if (next.port === undefined) next.port = 53
    return next
  }))
  const setNumber = (key: 'intervalS' | 'timeoutMs' | 'failuresToDown' | 'successesToUp', text: string) => setProbe((current) => {
    const next = { ...current }
    if (digits(text)) next[key] = Number(digits(text))
    else delete next[key]
    return next
  })
  const hasGateway = targets.some((target) => target.address === 'gateway')
  return (
    <ToggleField
      title={t('network.uplink.enable')}
      description={t('network.uplink.description')}
      control={<Switch checked={probe.enabled} aria-label={t('network.uplink.enable')} onCheckedChange={(on) => onChange(switchProbe(entry, on))} />}
    >
      {probe.enabled ? (
        <div className="grid gap-4">
          <p className="text-sm">{t('network.uplink.contactNotice')}</p>
          <FormField label={t('network.uplink.targets')} hint={problem ? <span className="text-destructive">{t(problem.key, problem.params)}</span> : t('network.uplink.targetsHint')}>
            {() => (
              <div className="grid gap-2" role="group" aria-label={t('network.uplink.targets')}>
                {targets.map((target, index) => (
                  <div key={index} className="flex flex-wrap items-center gap-2">
                    <Input className="w-40 font-mono" aria-label={t('network.uplink.address')} value={target.address} onChange={(event) => setTarget(index, { address: event.target.value.trim() })} />
                    <Select value={target.method} onValueChange={(value) => setTarget(index, { method: value as ProbeTarget['method'] })}>
                      <SelectTrigger className="w-36" aria-label={t('network.uplink.method')}><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="tcp">{t('network.uplink.methods.tcp')}</SelectItem>
                        <SelectItem value="icmp">{t('network.uplink.methods.icmp')}</SelectItem>
                      </SelectContent>
                    </Select>
                    {target.method === 'tcp' ? <Input className="w-24 font-mono" inputMode="numeric" aria-label={t('network.uplink.port')} value={target.port ?? ''} onChange={(event) => setTarget(index, { port: digits(event.target.value) ? Number(digits(event.target.value)) : undefined })} /> : null}
                    <Button size="icon-sm" variant="ghost" aria-label={t('network.uplink.removeTarget', { address: target.address })} onClick={() => setTargets(targets.filter((_, at) => at !== index))}><X /></Button>
                  </div>
                ))}
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" variant="outline" disabled={targets.length >= MAX_TARGETS} onClick={() => setTargets([...targets, { address: '', method: 'tcp', port: 53 }])}><Plus />{t('network.uplink.addTarget')}</Button>
                  {hasGateway ? null : <Button size="sm" variant="outline" disabled={targets.length >= MAX_TARGETS} onClick={() => setTargets([...targets, { address: 'gateway', method: 'icmp' }])}><Plus />{t('network.uplink.addGateway')}</Button>}
                </div>
              </div>
            )}
          </FormField>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField label={t('network.uplink.interval')}>
              {(id) => <Input id={id} className="w-28 font-mono" inputMode="numeric" value={entry.probe?.intervalS ?? ''} placeholder={String(probe.intervalS)} onChange={(event) => setNumber('intervalS', event.target.value)} />}
            </FormField>
            <FormField label={t('network.uplink.timeout')}>
              {(id) => <Input id={id} className="w-28 font-mono" inputMode="numeric" value={entry.probe?.timeoutMs ?? ''} placeholder={String(probe.timeoutMs)} onChange={(event) => setNumber('timeoutMs', event.target.value)} />}
            </FormField>
            <FormField label={t('network.uplink.failuresToDown')}>
              {(id) => <Input id={id} className="w-28 font-mono" inputMode="numeric" value={entry.probe?.failuresToDown ?? ''} placeholder={String(probe.failuresToDown)} onChange={(event) => setNumber('failuresToDown', event.target.value)} />}
            </FormField>
            <FormField label={t('network.uplink.successesToUp')}>
              {(id) => <Input id={id} className="w-28 font-mono" inputMode="numeric" value={entry.probe?.successesToUp ?? ''} placeholder={String(probe.successesToUp)} onChange={(event) => setNumber('successesToUp', event.target.value)} />}
            </FormField>
          </div>
          <p className="text-sm text-muted-foreground">
            {t('network.uplink.timing', { down: probe.failuresToDown * probe.intervalS, up: probe.successesToUp * probe.intervalS })}{' '}
            {t('network.uplink.cost', { megabytes: megabytesPerDay(probe.intervalS) })}
          </p>
        </div>
      ) : null}
    </ToggleField>
  )
}
