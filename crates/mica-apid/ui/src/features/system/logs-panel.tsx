import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { RefreshCcw, ScrollText } from 'lucide-react'
import { api } from '@/shared/lib/http'
import { type Feature, useFeatures } from '@/shared/lib/features'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { Panel } from '@/shared/components/panel'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'

/// The logs the device lets the console read, and the feature each belongs to.
const SOURCES: readonly { name: string; feature?: Feature }[] = [
  { name: 'micad' },
  { name: 'apid' },
  { name: 'time' },
  { name: 'ssh', feature: 'ssh' },
  { name: 'wifi-client', feature: 'wifi' },
  { name: 'wifi-ap', feature: 'wifi' },
  { name: 'bluetooth', feature: 'bluetooth' },
  { name: 'mqtt-broker', feature: 'mqtt' },
  { name: 'mqttd', feature: 'mqtt' },
]

/// `GET /api/v1/system/logs/{source}`: the newest lines, already scrubbed by the device.
interface ServiceLog {
  source: string
  available: boolean
  lines?: string[]
  truncated?: boolean
  detail?: string
}

/// A read-only view of one service's log: read when asked, never followed.
export function LogsPanel() {
  const { t } = useTranslation()
  const serves = useFeatures()
  const sources = SOURCES.filter((source) => source.feature === undefined || serves(source.feature))
  const [source, setSource] = useState('micad')
  const log = useQuery({
    queryKey: ['system-log', source],
    queryFn: () => api<ServiceLog>(`/api/v1/system/logs/${encodeURIComponent(source)}`),
    retry: false,
  })
  return (
    <Panel
      title={t('system.logs.title')}
      description={t('system.logs.description')}
      action={<ScrollText className="size-5 text-muted-foreground" />}
    >
      <div className="flex flex-wrap items-center gap-2">
        <Select value={source} onValueChange={(value) => setSource(String(value))}>
          <SelectTrigger className="w-48" aria-label={t('system.logs.source')}><SelectValue /></SelectTrigger>
          <SelectContent>{sources.map((option) => <SelectItem key={option.name} value={option.name}>{t(`system.logs.sources.${option.name}`)}</SelectItem>)}</SelectContent>
        </Select>
        <Button type="button" variant="outline" size="sm" onClick={() => void log.refetch()} disabled={log.isFetching}>
          {log.isFetching ? <Spinner /> : <RefreshCcw />}
          {t('system.logs.refresh')}
        </Button>
      </div>
      {log.error ? <Callout tone="danger" title={failureDetail(log.error, t('common.requestFailed'))} /> : null}
      {log.data && !log.data.available ? <Callout tone="neutral" title={t('system.logs.unavailable', { detail: log.data.detail ?? '' })} /> : null}
      {log.data?.available ? (
        <>
          {log.data.truncated ? <p className="text-sm text-muted-foreground">{t('system.logs.truncated')}</p> : null}
          <pre aria-label={t('system.logs.lines')} className="max-h-96 overflow-auto rounded-md border bg-muted/40 p-3 font-mono text-xs whitespace-pre-wrap break-all">
            {(log.data.lines ?? []).length > 0 ? (log.data.lines ?? []).join('\n') : t('system.logs.empty')}
          </pre>
        </>
      ) : null}
    </Panel>
  )
}
