import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { MdnsOverview, SsdpOverview, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { CollectionPanel } from '@/shared/components/panel'
import { RowItem, RowList } from '@/shared/components/row-item'
import { StatusBadge } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { Switch } from '@/shared/components/ui/switch'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

/// What the device announces on the LAN: the mDNS and SSDP switches, and
/// every mDNS service with its source, read-only. Each switch reads the
/// effective state, since its setting is absent while the product default
/// holds.
export function LanDiscoveryPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const mdns = useQuery({ queryKey: ['mdns'], queryFn: () => api<MdnsOverview>('/api/v1/mdns'), refetchInterval: 10_000 })
  const toggle = useMutationFeedback<TaskAccepted, boolean>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings/access.mdns.enabled', json('PUT', value)),
    success: (_data, value) => t(value ? 'overview.discovery.enabled' : 'overview.discovery.disabled'),
    failure: t('overview.discovery.mdns'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['mdns'] }),
  })
  const ssdp = useQuery({ queryKey: ['ssdp'], queryFn: () => api<SsdpOverview>('/api/v1/ssdp'), refetchInterval: 10_000 })
  const ssdpToggle = useMutationFeedback<TaskAccepted, boolean>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings/access.ssdp.enabled', json('PUT', value)),
    success: (_data, value) => t(value ? 'overview.discovery.ssdpEnabled' : 'overview.discovery.ssdpDisabled'),
    failure: t('overview.discovery.ssdp'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['ssdp'] }),
  })
  const state = mdns.data?.state
  const source = state?.source === 'setting' ? 'setting' : 'product'
  const upnp = ssdp.data?.state
  const upnpSource = upnp?.source === 'setting' ? 'setting' : 'product'
  const unavailable = [mdns, ssdp]
    .map((query) => query.error ? t('common.requestFailed') : query.data && !query.data.available ? query.data.error ?? t('common.notAvailable') : undefined)
    .find((reason) => reason !== undefined)

  return (
    <CollectionPanel title={t('overview.discovery.title')}>
      <RowList>
        <RowItem
          title={t('overview.discovery.mdns')}
          description={t('overview.discovery.mdnsCopy', {
            hostname: state?.hostname ?? t('common.notAvailable'),
            source: t(`overview.discovery.source.${source}`),
          })}
          actions={<Switch checked={state?.enabled === true} onCheckedChange={(value) => toggle.mutate(value)} disabled={state === undefined || toggle.isPending} aria-label={t('overview.discovery.mdns')} />}
        />
        <RowItem
          title={t('overview.discovery.ssdp')}
          description={upnp?.enabled && !upnp.active && upnp.reason
            ? t('overview.discovery.ssdpHeld', { reason: upnp.reason })
            : t('overview.discovery.ssdpCopy', { source: t(`overview.discovery.source.${upnpSource}`) })}
          actions={<Switch checked={upnp?.enabled === true} onCheckedChange={(value) => ssdpToggle.mutate(value)} disabled={upnp === undefined || ssdpToggle.isPending} aria-label={t('overview.discovery.ssdp')} />}
        />
        {state?.services.map((service) => (
          <RowItem
            key={`${service.instance}.${service.type}`}
            title={<span className="font-mono text-[0.8125rem] break-all">{`${service.instance}.${service.type}`}</span>}
            description={t('overview.discovery.service', { port: service.port, source: service.source })}
            actions={<StatusBadge tone={service.published ? 'success' : 'neutral'}>{t(service.published ? 'overview.discovery.published' : 'overview.discovery.notPublished')}</StatusBadge>}
          />
        ))}
      </RowList>
      <div className="px-3 pb-3"><TaskProgress taskId={toggle.data?.taskId ?? ssdpToggle.data?.taskId} /></div>
      {state?.refused.map((refused) => (
        <div key={refused.source} className="px-3 pb-3">
          <Callout tone="warning" title={t('overview.discovery.refused', { source: refused.source, error: refused.error })} />
        </div>
      ))}
      {unavailable ? <div className="p-3"><Callout tone="danger" title={unavailable} /></div> : null}
    </CollectionPanel>
  )
}
