import { useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Cog } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, UnitEntry, UserUnit } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { DataTable } from '@/shared/components/data-table'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { CollectionPanel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Switch } from '@/shared/components/ui/switch'
import { Textarea } from '@/shared/components/ui/textarea'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { TEMPLATES, UNITS_SETTINGS, unitRoute, unitTone, useDeclaredUnits, useUnits } from './units'

/// The user's own services: each declared one with its unit file, and each
/// one put on the device with `mica-unit` by hand.
export function UnitsPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const units = useUnits()
  const declared = useDeclaredUnits()
  const [adding, setAdding] = useState<{ name: string } & UserUnit>()
  const init = units.data?.init ?? 'systemd'

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['units'] })
    void queryClient.invalidateQueries({ queryKey: ['settings', UNITS_SETTINGS] })
  }
  const toggle = useMutationFeedback<TaskAccepted, { name: string; unit: UserUnit }>({
    mutationFn: ({ name, unit }) => api<TaskAccepted>(unitRoute(name), json('PUT', unit)),
    success: (_data, { name, unit }) => t(unit.enabled ? 'services.enabledToast' : 'services.disabledToast', { name }),
    failure: t('services.units.title'),
    onSuccess: invalidate,
  })
  const save = () => {
    if (!adding) return Promise.resolve()
    const { name, ...unit } = adding
    return api<TaskAccepted>(unitRoute(name.trim()), json('PUT', unit)).then(() => {
      setAdding(undefined)
      invalidate()
    })
  }

  const enable = (row: UnitEntry) => {
    const unit = declared.data?.[row.name]
    if (!row.managed || !unit) return null
    return (
      <Switch
        checked={unit.enabled}
        onCheckedChange={(value) => toggle.mutate({ name: row.name, unit: { ...unit, enabled: value } })}
        disabled={toggle.isPending}
        aria-label={row.name}
      />
    )
  }

  return (
    <div className="grid gap-3">
      {units.error ? <Callout tone="danger" title={failureDetail(units.error, t('common.requestFailed'))} /> : null}
      {units.data?.available === false ? <Callout tone="neutral" title={t('services.units.noMicaUnit')} /> : null}
      <CollectionPanel
        title={t('services.units.title')}
        description={t(`services.units.copy.${init}`)}
        action={units.data?.available ? (
          <Button size="sm" variant="outline" onClick={() => setAdding({ name: '', kind: init, text: TEMPLATES[init], enabled: true })}>{t('services.units.add')}</Button>
        ) : undefined}
      >
        <DataTable<UnitEntry>
          rows={units.data?.units}
          rowKey={(row) => row.name}
          isPending={units.isPending}
          empty={t('services.units.empty')}
          emptyIcon={<Cog />}
          columns={[
            { id: 'name', header: t('services.units.name'), cell: (row) => (
              <Link to="/services/units/$name" params={{ name: row.name }} className="font-mono font-medium text-primary hover:underline">{row.name}</Link>
            ) },
            { id: 'state', header: t('services.units.state'), cell: (row) => (
              <span className="flex flex-wrap items-center gap-2">
                <StatusBadge tone={unitTone(row)}>{row.installed ? row.active ?? t('common.states.unknown') : t('services.units.notInstalled')}</StatusBadge>
                {row.declared ? null : <span className="text-muted-foreground text-xs">{t('services.units.byHand')}</span>}
                {row.refused ? <span className="text-muted-foreground text-xs">{row.refused}</span> : null}
              </span>
            ) },
            { id: 'enabled', header: t('services.units.boot'), align: 'end', cell: (row) => enable(row) ?? (
              <span className="text-muted-foreground text-sm">{t(row.enabled ? 'common.states.enabled' : 'common.states.disabled')}</span>
            ) },
          ]}
        />
      </CollectionPanel>

      <FormDialog
        open={adding !== undefined}
        onOpenChange={(next) => { if (!next) setAdding(undefined) }}
        title={t('services.units.add')}
        description={t('services.units.addCopy')}
        submitLabel={t('common.actions.save')}
        success={t('services.units.saved', { name: adding?.name.trim() ?? '' })}
        failure={t('services.units.title')}
        onSubmit={save}
        size="lg"
      >
        {adding ? <UnitFields value={adding} onChange={setAdding} withName /> : null}
      </FormDialog>
    </div>
  )
}

/// The fields of one service: its name when it is new, its text, the
/// script's `conf.d` settings on OpenRC, and whether it starts at boot.
export function UnitFields<Value extends UserUnit & { name?: string }>({ value, onChange, withName = false }: {
  value: Value
  onChange: (next: Value) => void
  withName?: boolean
}) {
  const { t } = useTranslation()
  return (
    <>
      {withName ? (
        <FormField label={t('services.units.name')} hint={t(`services.units.nameHint.${value.kind}`)}>
          {(id) => <Input id={id} className="font-mono" value={value.name ?? ''} onChange={(event) => onChange({ ...value, name: event.target.value })} required spellCheck={false} />}
        </FormField>
      ) : null}
      <FormField label={t(`services.units.text.${value.kind}`)} hint={t(`services.units.textHint.${value.kind}`)}>
        {(id) => <Textarea id={id} className="min-h-64 font-mono text-[0.8125rem]" value={value.text} onChange={(event) => onChange({ ...value, text: event.target.value })} spellCheck={false} required />}
      </FormField>
      {value.kind === 'openrc' ? (
        <FormField label={t('services.units.conf')} hint={t('services.units.confHint')}>
          {(id) => (
            <Textarea
              id={id}
              className="min-h-24 font-mono text-[0.8125rem]"
              value={value.conf ?? ''}
              onChange={(event) => onChange({ ...value, conf: event.target.value === '' ? undefined : event.target.value })}
              spellCheck={false}
            />
          )}
        </FormField>
      ) : null}
      <ToggleField
        title={t('services.units.enabled')}
        description={t('services.units.enabledHint')}
        control={<Switch checked={value.enabled} onCheckedChange={(enabled) => onChange({ ...value, enabled })} aria-label={t('services.units.enabled')} />}
      />
    </>
  )
}
