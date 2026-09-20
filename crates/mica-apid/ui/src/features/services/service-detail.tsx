import { Link, useParams } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ChevronLeft } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { MetricCard } from '@/shared/components/metric-card'
import { Page, PageHeader, PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { buttonVariants } from '@/shared/components/ui/button'
import { Switch } from '@/shared/components/ui/switch'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { failureDetail } from '@/shared/feedback/toast'
import { SimulationNotice } from '@/shared/simulation/simulation-notice'
import { findService, serviceEndpoint } from './service-catalog'
import { MqttPanel } from './mqtt-panel'
import { ContainersPanel } from './containers-panel'

export function ServiceDetailPage() {
  const { service: id } = useParams({ from: '/services_/$service' })
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const service = findService(id)

  const enabled = useQuery({
    queryKey: ['settings', service?.settingsPath],
    queryFn: () => api<boolean>(`/api/v1/settings/${service?.settingsPath}`),
    enabled: Boolean(service?.settingsPath),
  })
  const state = useQuery({
    queryKey: ['state', service?.statePath],
    queryFn: () => api<Record<string, unknown>>(`/api/v1/state/${service?.statePath}`),
    enabled: Boolean(service?.statePath),
    retry: false,
  })
  const title = service ? t(`services.${service.id}.title`) : id
  const update = useMutationFeedback<TaskAccepted, boolean>({
    mutationFn: (value) => api<TaskAccepted>(`/api/v1/settings/${service?.settingsPath}`, json('PUT', value)),
    success: (_data, value) => t(value ? 'services.enabledToast' : 'services.disabledToast', { name: title }),
    failure: title,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['settings', service?.settingsPath] }),
  })

  if (!service) {
    return <Page><Panel><p className="py-6 text-center text-sm text-muted-foreground">{t('services.unknown', { id })}</p></Panel></Page>
  }

  const Icon = service.icon
  const on = enabled.data === true
  const observed = typeof state.data?.state === 'string' ? state.data.state : undefined
  const endpoint = serviceEndpoint(state.data)

  return (
    <Page>
      <PageHeader
        title={title}
        description={t(`services.${service.id}.warning`)}
        media={<Icon />}
        back={<Link to="/services" className={buttonVariants({ variant: 'outline', size: 'sm' })}><ChevronLeft aria-hidden="true" />{t('services.title')}</Link>}
        action={(
          <>
            <span className="text-sm font-medium">{t(on ? 'common.states.enabled' : 'common.states.disabled')}</span>
            <Switch
              checked={on}
              onCheckedChange={(value) => update.mutate(value)}
              disabled={enabled.isPending || enabled.isError || update.isPending}
              aria-label={title}
            />
          </>
        )}
      />

      {enabled.error ? <Callout tone="danger" title={failureDetail(enabled.error, t('common.requestFailed'))} /> : null}

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <MetricCard
          label={t('services.detail.configured')}
          value={t(on ? 'common.states.enabled' : 'common.states.disabled')}
          caption={update.isPending ? t('services.saving') : t('services.detail.settled')}
        />
        <MetricCard
          label={t('services.detail.observed')}
          value={observed ?? t('common.states.unknown')}
          caption={state.isError ? t('services.noLiveState') : t('services.liveAvailable')}
        />
        <MetricCard
          label={t('services.detail.endpoint')}
          mono
          value={<span className="text-sm break-all">{endpoint ?? t('common.notAvailable')}</span>}
          caption={endpoint ? t('services.detail.endpointReported') : t('services.detail.endpointUnreported')}
        />
      </div>

      <PageSection title={t('services.detail.configuration')}>
        {service.id === 'mqtt' ? <MqttPanel /> : <ContainersPanel />}
      </PageSection>

      <SimulationNotice scope={title} />
    </Page>
  )
}
