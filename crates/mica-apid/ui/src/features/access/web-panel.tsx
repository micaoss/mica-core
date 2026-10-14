import { useEffect, useState, type FormEvent } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted, WebConfiguration } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

/// How long the page waits for apid to come back on its new listener.
const MOVE_DELAY_MS = 4000

/// Where the console answers after `web` takes effect, on this host.
export function consoleUrl(web: WebConfiguration, hostname: string) {
  return web.httpsEnabled ? `https://${hostname}:${web.httpsPort}/` : `http://${hostname}:${web.httpPort}/`
}

/// Where the console listens. A save restarts apid on the new listeners, so
/// this page cannot stay: it says where the console went and follows it there.
export function WebPanel() {
  const { t } = useTranslation()
  const web = useQuery({ queryKey: ['web'], queryFn: () => api<WebConfiguration>('/api/v1/web') })
  const [httpPort, setHttpPort] = useState<string>()
  const [httpsEnabled, setHttpsEnabled] = useState<boolean>()
  const [httpsPort, setHttpsPort] = useState<string>()
  const [movingTo, setMovingTo] = useState<string>()

  const stored = web.data
  const current: WebConfiguration = {
    httpPort: Number(httpPort ?? stored?.httpPort ?? 8080),
    httpsEnabled: httpsEnabled ?? stored?.httpsEnabled ?? false,
    httpsPort: Number(httpsPort ?? stored?.httpsPort ?? 8443),
  }

  const write = useMutationFeedback<TaskAccepted, WebConfiguration>({
    mutationFn: (body) => api<TaskAccepted>('/api/v1/web', json('PUT', body)),
    success: t('access.web.saved'),
    failure: t('access.web.save'),
    onSuccess: (_task, body) => setMovingTo(consoleUrl(body, window.location.hostname)),
  })

  useEffect(() => {
    if (!movingTo) return
    const timer = window.setTimeout(() => window.location.assign(movingTo), MOVE_DELAY_MS)
    return () => window.clearTimeout(timer)
  }, [movingTo])

  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (stored) write.mutate(current)
  }

  return (
    <Panel>
      <form className="grid gap-4" onSubmit={submit}>
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('access.web.httpPort')} hint={t('access.web.httpPortHint')}>
            {(id) => <Input id={id} type="number" min={1} max={65535} value={current.httpPort.toString()} onChange={(event) => setHttpPort(event.target.value)} required />}
          </FormField>
          <FormField label={t('access.web.httpsPort')} hint={t('access.web.httpsPortHint')}>
            {(id) => <Input id={id} type="number" min={1} max={65535} value={current.httpsPort.toString()} onChange={(event) => setHttpsPort(event.target.value)} disabled={!current.httpsEnabled} required />}
          </FormField>
          <FormField className="sm:col-span-2" label={t('access.web.https')} hint={t('access.web.httpsHint')}>
            {(id) => <Switch id={id} checked={current.httpsEnabled} onCheckedChange={(value) => setHttpsEnabled(value)} aria-label={t('access.web.https')} />}
          </FormField>
        </div>
        {web.error ? <Callout tone="danger" title={failureDetail(web.error, t('common.requestFailed'))} /> : null}
        {movingTo ? (
          <Callout tone="neutral" title={t('access.web.moving', { url: movingTo })}>
            <a className="font-mono underline" href={movingTo}>{t('access.web.open', { url: movingTo })}</a>
          </Callout>
        ) : null}
        <Button className="justify-self-end" type="submit" disabled={write.isPending || !stored || movingTo !== undefined}>
          {write.isPending ? <Spinner /> : null}{t('access.web.save')}
        </Button>
      </form>
    </Panel>
  )
}
