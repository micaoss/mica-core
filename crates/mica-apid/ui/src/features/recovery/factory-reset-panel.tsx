import { useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField } from '@/shared/components/form-field'
import { Input } from '@/shared/components/ui/input'
import { Panel } from '@/shared/components/panel'
import { failureDetail } from '@/shared/feedback/toast'
import { formatDeviceClock } from '@/shared/components/fact'

/// `GET /api/v1/system/factory-reset`.
interface FactoryResetStatus {
  supported: boolean
  refusal: { code: string; message: string } | null
  last: { requested: number; completed: number; by: string; source: string; unremoved: number } | null
}

const ERASED = ['claim', 'settings', 'apps', 'data'] as const
const KEPT = ['software', 'deviceCode'] as const

/// The factory reset: what it erases and keeps, and the administrator
/// password it asks for whatever the session.
export function FactoryResetPanel() {
  const { t, i18n } = useTranslation()
  const status = useQuery({
    queryKey: ['factory-reset'],
    queryFn: () => api<FactoryResetStatus>('/api/v1/system/factory-reset'),
  })
  const [open, setOpen] = useState(false)
  const [password, setPassword] = useState('')
  const [staged, setStaged] = useState(false)
  // The dialog reports the outcome; this mutation keeps the page in the
  // rebooting state once the device has accepted.
  const reset = useMutation({
    mutationFn: () => api('/api/v1/system/factory-reset', json('POST', { password })),
    onSuccess: () => {
      setStaged(true)
      setPassword('')
    },
  })
  const refusal = status.data?.refusal
  const last = status.data?.last
  return (
    <Panel>
      <Callout tone="warning" title={t('system.factoryReset.summary')} />
      <div className="grid gap-4 text-sm sm:grid-cols-2">
        <div>
          <p className="font-medium">{t('system.factoryReset.erasedTitle')}</p>
          <ul className="list-disc pl-5 text-muted-foreground">
            {ERASED.map((item) => <li key={item}>{t(`system.factoryReset.erased.${item}`)}</li>)}
          </ul>
        </div>
        <div>
          <p className="font-medium">{t('system.factoryReset.keptTitle')}</p>
          <ul className="list-disc pl-5 text-muted-foreground">
            {KEPT.map((item) => <li key={item}>{t(`system.factoryReset.kept.${item}`)}</li>)}
          </ul>
        </div>
      </div>
      <Callout title={t('system.factoryReset.notAWipe')} />
      {last ? (
        <p className="text-sm text-muted-foreground">
          {t('system.factoryReset.last', {
            time: formatDeviceClock(last.completed, i18n.resolvedLanguage ?? 'en'),
            by: t(`system.factoryReset.by.${last.by}`, { defaultValue: last.by }),
          })}
        </p>
      ) : null}
      {staged ? (
        <Callout tone="warning" title={t('system.factoryReset.rebooting')}>{t('system.factoryReset.afterwards')}</Callout>
      ) : refusal ? (
        <Callout title={t(`system.factoryReset.refusals.${refusal.code}`, { defaultValue: refusal.message })} />
      ) : null}
      {status.error ? <Callout tone="danger" title={failureDetail(status.error, t('common.requestFailed'))} /> : null}
      <Button
        className="justify-self-start"
        variant="destructive"
        disabled={!status.data || Boolean(refusal) || staged}
        onClick={() => setOpen(true)}
      >
        {t('system.factoryReset.start')}
      </Button>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title={t('system.factoryReset.confirmTitle')}
        description={t('system.factoryReset.confirm')}
        submitLabel={t('system.factoryReset.submit')}
        success={t('system.factoryReset.staged')}
        failure={t('system.factoryReset.start')}
        onSubmit={() => reset.mutateAsync()}
      >
        <FormField label={t('system.factoryReset.password')}>
          {(id) => <Input id={id} type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} required />}
        </FormField>
      </FormDialog>
    </Panel>
  )
}
