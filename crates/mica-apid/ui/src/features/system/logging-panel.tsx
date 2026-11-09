import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { FileCog } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import { useOpenrc } from '@/shared/lib/features'
import type { TaskAccepted } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { TaskProgress } from '@/shared/components/task-progress'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { failureDetail } from '@/shared/feedback/toast'

/// `logging.remote`: a collector, kept while forwarding is off.
interface RemoteLog {
  enabled: boolean
  address: string
  port: number
  protocol: 'udp' | 'tcp'
}

/// `GET /api/v1/settings/logging`: what was set; an absent value follows the product.
interface LoggingSettings {
  persistent?: boolean | null
  maxSizeKiB?: number | null
  remote: RemoteLog
}

/// `GET /api/v1/state/logging`: what Base applies.
interface LoggingState {
  supported?: boolean
  detail?: string
  persistent?: boolean
  persistentSource?: 'setting' | 'default'
  maxSizeKiB?: number
  maxSizeKiBLimit?: number | null
}

type Persistence = 'default' | 'on' | 'off'

interface Draft {
  persistence: Persistence
  cap: string
  remote: RemoteLog
}

const NO_REMOTE: RemoteLog = { enabled: false, address: '', port: 514, protocol: 'udp' }

function draftOf(settings: LoggingSettings | undefined): Draft {
  const persistent = settings?.persistent
  return {
    persistence: persistent === true ? 'on' : persistent === false ? 'off' : 'default',
    cap: settings?.maxSizeKiB == null ? '' : String(settings.maxSizeKiB),
    remote: settings?.remote ?? NO_REMOTE,
  }
}

/// Whether the log is kept across boots, its cap, and a collector it is forwarded to.
export function LoggingPanel() {
  const { t } = useTranslation()
  const openrc = useOpenrc()
  const queryClient = useQueryClient()
  const settings = useQuery({ queryKey: ['settings', 'logging'], queryFn: () => api<LoggingSettings>('/api/v1/settings/logging') })
  const state = useQuery({ queryKey: ['state', 'logging'], queryFn: () => api<LoggingState>('/api/v1/state/logging'), retry: false })
  const [edited, setEdited] = useState<Draft>()
  const draft = edited ?? draftOf(settings.data)
  const edit = (change: Partial<Draft>) => setEdited({ ...draft, ...change })
  const editRemote = (change: Partial<RemoteLog>) => edit({ remote: { ...draft.remote, ...change } })
  const save = useMutationFeedback<TaskAccepted, Draft>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings', json('PATCH', {
      'logging.persistent': value.persistence === 'default' ? null : value.persistence === 'on',
      'logging.maxSizeKiB': value.cap.trim() === '' ? null : Number(value.cap),
      'logging.remote': value.remote,
    })),
    success: t('system.logging.saved'),
    failure: t('system.logging.save'),
    onSuccess: () => {
      setEdited(undefined)
      void queryClient.invalidateQueries({ queryKey: ['settings', 'logging'] })
      void queryClient.invalidateQueries({ queryKey: ['state', 'logging'] })
    },
  })
  const live = state.data
  const productDefault = live?.persistentSource === 'default' && live.persistent !== undefined
    ? t(live.persistent ? 'system.logging.defaultOn' : 'system.logging.defaultOff')
    : t('system.logging.default')
  return (
    <Panel title={t('system.logging.title')} description={t('system.logging.description')} action={<FileCog className="size-5 text-muted-foreground" />}>
      {live?.supported === false ? <Callout tone="neutral" title={t('system.logging.unsupported', { detail: live.detail ?? '' })} /> : null}
      <form className="grid gap-4" onSubmit={(event) => { event.preventDefault(); save.mutate(draft) }}>
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('system.logging.persistent')} hint={t('system.logging.persistentHint')}>
            {(id) => (
              <Select value={draft.persistence} onValueChange={(value) => edit({ persistence: value as Persistence })}>
                <SelectTrigger id={id} className="w-full"><SelectValue>{draft.persistence === 'default' ? productDefault : t(`system.logging.${draft.persistence}`)}</SelectValue></SelectTrigger>
                <SelectContent>
                  <SelectItem value="default">{productDefault}</SelectItem>
                  <SelectItem value="on">{t('system.logging.on')}</SelectItem>
                  <SelectItem value="off">{t('system.logging.off')}</SelectItem>
                </SelectContent>
              </Select>
            )}
          </FormField>
          <FormField
            label={t('system.logging.cap')}
            hint={live?.maxSizeKiBLimit ? t('system.logging.capHintLimit', { limit: live.maxSizeKiBLimit }) : t('system.logging.capHint')}
          >
            {(id) => <Input id={id} inputMode="numeric" value={draft.cap} placeholder={live?.maxSizeKiB ? String(live.maxSizeKiB) : ''} onChange={(event) => edit({ cap: event.target.value })} />}
          </FormField>
        </div>
        <ToggleField
          title={t('system.logging.forward')}
          description={t('system.logging.forwardHint')}
          control={<Switch checked={draft.remote.enabled} onCheckedChange={(enabled) => editRemote({ enabled })} aria-label={t('system.logging.forward')} />}
        />
        <Callout tone="warning" title={t('system.logging.warning')} />
        <div className="grid gap-4 sm:grid-cols-3">
          <FormField label={t('system.logging.address')} hint={t('system.logging.addressHint')}>
            {(id) => <Input id={id} className="font-mono" value={draft.remote.address} placeholder="192.0.2.10" onChange={(event) => editRemote({ address: event.target.value.trim() })} />}
          </FormField>
          <FormField label={t('system.logging.port')}>
            {(id) => <Input id={id} inputMode="numeric" value={String(draft.remote.port)} onChange={(event) => editRemote({ port: Number(event.target.value) || 0 })} />}
          </FormField>
          <FormField label={t('system.logging.protocol')} hint={openrc ? t('system.logging.udpOnly') : undefined}>
            {(id) => (
              <Select value={draft.remote.protocol} onValueChange={(value) => editRemote({ protocol: value as RemoteLog['protocol'] })}>
                <SelectTrigger id={id} className="w-full"><SelectValue>{draft.remote.protocol.toUpperCase()}</SelectValue></SelectTrigger>
                <SelectContent>
                  <SelectItem value="udp">UDP</SelectItem>
                  {openrc ? null : <SelectItem value="tcp">TCP</SelectItem>}
                </SelectContent>
              </Select>
            )}
          </FormField>
        </div>
        <Button className="justify-self-end" type="submit" disabled={save.isPending || edited === undefined}>{t('system.logging.save')}</Button>
        <TaskProgress taskId={save.data?.taskId} />
        {settings.error ? <Callout tone="danger" title={failureDetail(settings.error, t('common.requestFailed'))} /> : null}
      </form>
    </Panel>
  )
}
