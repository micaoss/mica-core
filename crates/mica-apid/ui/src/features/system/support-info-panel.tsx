import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ClipboardCopy, Download, LifeBuoy } from 'lucide-react'
import { api } from '@/shared/lib/http'
import type { SupportInfo } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { Panel } from '@/shared/components/panel'
import { formatBytes } from '@/shared/components/fact'
import { notifyFailure, notifySuccess, failureDetail } from '@/shared/feedback/toast'

type T = ReturnType<typeof useTranslation>['t']

/// Copy or download the device's support summary. Both read it afresh, and
/// nothing is sent anywhere: the operator decides where it goes.
export function SupportInfoPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const read = () => queryClient.fetchQuery({ queryKey: ['support-info'], queryFn: () => api<SupportInfo>('/api/v1/system/support-info'), staleTime: 0, retry: false })
  const copy = useMutation({
    mutationFn: async () => {
      const text = supportInfoText(await read(), t)
      try {
        await navigator.clipboard.writeText(text)
      } catch (error) {
        notifyFailure(t('common.copyFailed'), error instanceof Error ? error.message : undefined)
        return
      }
      notifySuccess(t('system.support.copied'))
    },
  })
  const download = useMutation({
    mutationFn: async () => {
      const summary = await read()
      const url = URL.createObjectURL(new Blob([`${JSON.stringify(summary, null, 2)}\n`], { type: 'application/json' }))
      const link = document.createElement('a')
      link.href = url
      link.download = `mica-support-${summary.generatedAt.replace(/[-:]/g, '')}.json`
      link.click()
      URL.revokeObjectURL(url)
    },
  })
  const error = copy.error ?? download.error
  return (
    <Panel title={t('system.support.title')} description={t('system.support.description')} action={<LifeBuoy className="size-5 text-muted-foreground" />}>
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" onClick={() => { download.reset(); copy.mutate() }} disabled={copy.isPending}><ClipboardCopy />{t('system.support.copy')}</Button>
        <Button variant="outline" onClick={() => { copy.reset(); download.mutate() }} disabled={download.isPending}><Download />{t('system.support.download')}</Button>
      </div>
      {error ? <Callout tone="danger" title={failureDetail(error, t('common.requestFailed'))} /> : null}
    </Panel>
  )
}

/// The summary as labelled lines in a fixed order. Every value is made one
/// line here as well, so nothing it carries can pass for a line of its own.
export function supportInfoText(summary: SupportInfo, t: T) {
  const line = (label: string, value: string) => `${t(`system.support.labels.${label}`)}: ${value}`
  const text = (value: string | number | null | undefined) =>
    value === null || value === undefined || value === '' ? t('common.notAvailable') : String(value).replace(/\p{Cc}/gu, ' ')
  const absent = (section: { available: boolean, reason?: string }) => t('system.support.unavailable', { reason: text(section.reason) })
  const lines = [t('system.support.heading'), line('generated', text(summary.generatedAt))]
  const { device, software, deployment, time, integrity, data, lastUpdateError } = summary
  if (device.available) lines.push(line('board', text(device.board)), line('kernel', text(device.kernel)))
  else lines.push(line('board', absent(device)), line('kernel', absent(device)))
  if (software.available) {
    lines.push(
      line('product', [software.product, software.productVersion].map(text).join(' ')),
      line('osVersion', text(software.osVersion)),
      line('core', text(software.core)),
      line('daemon', text(software.daemon)),
    )
  } else {
    lines.push(line('product', absent(software)))
  }
  lines.push(line('deployment', deployment.available
    ? t(deployment.confirmed ? 'system.support.deploymentConfirmed' : 'system.support.deploymentTrial', { id: text(deployment.id), version: text(deployment.version), generation: text(deployment.generation) })
    : absent(deployment)))
  lines.push(line('uptime', summary.uptimeSeconds === null ? t('common.notAvailable') : t('system.support.seconds', { seconds: summary.uptimeSeconds })))
  if (time.available) lines.push(line('timezone', text(time.timezone)), line('timeSync', text(time.status)))
  else lines.push(line('timezone', absent(time)), line('timeSync', absent(time)))
  for (const item of integrity.available ? integrity.items : []) {
    const state = t(`system.information.integrity.states.${item.state}`, { defaultValue: text(item.state) })
    const label = t(`system.information.integrity.items.${item.key}`, { defaultValue: text(item.key) })
    lines.push(`${text(label)}: ${state}${item.detailCode ? ` (${text(item.detailCode)})` : ''}`)
  }
  lines.push(line('data', data.available
    ? t('system.support.dataValue', {
      state: t(`system.information.integrity.states.${data.state}`, { defaultValue: text(data.state) }),
      used: data.usedBytes === null ? t('common.notAvailable') : formatBytes(data.usedBytes),
      total: data.totalBytes === null ? t('common.notAvailable') : formatBytes(data.totalBytes),
      percent: text(data.usedPercent),
    })
    : absent(data)))
  lines.push(line('lastUpdateError', !lastUpdateError.available
    ? absent(lastUpdateError)
    : lastUpdateError.code ? `${text(lastUpdateError.code)}${lastUpdateError.during ? ` (${text(lastUpdateError.during)})` : ''}` : t('system.support.none')))
  return lines.join('\n')
}
