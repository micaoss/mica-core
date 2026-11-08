import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted } from '@/lib/types'
import { PageSection } from '@/shared/components/page'
import { CollectionPanel } from '@/shared/components/panel'
import { RowItem, RowList } from '@/shared/components/row-item'
import { TaskProgress } from '@/shared/components/task-progress'
import { Switch } from '@/shared/components/ui/switch'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

/// The `cloudAdmins` live state: whether the cloud's SSH users are
/// administrators, what decided it, and the product's default.
interface CloudAdmins { enabled: boolean; source: 'setting' | 'product'; productDefault: boolean }

/// Whether the cloud's SSH users are administrators (sudo). Shown while the
/// product grants it or the setting is set, so a product without a cloud
/// shows nothing.
export function CloudAdminsSection() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const state = useQuery({ queryKey: ['state', 'cloudAdmins'], queryFn: () => api<CloudAdmins>('/api/v1/state/cloudAdmins') })
  const toggle = useMutationFeedback<TaskAccepted, boolean>({
    mutationFn: (value) => api<TaskAccepted>('/api/v1/settings/access.cloudAdmins.enabled', json('PUT', value)),
    success: (_data, value) => t(value ? 'access.cloudAdmins.granted' : 'access.cloudAdmins.revoked'),
    failure: t('access.cloudAdmins.switch'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['state', 'cloudAdmins'] }),
  })
  const admins = state.data
  if (!admins || (!admins.productDefault && admins.source !== 'setting')) return null
  return (
    <PageSection title={t('access.cloudAdmins.title')} description={t('access.cloudAdmins.description')} tone="danger">
      <CollectionPanel>
        <RowList>
          <RowItem
            title={t('access.cloudAdmins.switch')}
            description={t(`access.cloudAdmins.source.${admins.source}`)}
            actions={<Switch checked={admins.enabled} onCheckedChange={(value) => toggle.mutate(value)} disabled={toggle.isPending} aria-label={t('access.cloudAdmins.switch')} />}
          />
        </RowList>
        <div className="px-3 pb-3"><TaskProgress taskId={toggle.data?.taskId} /></div>
      </CollectionPanel>
    </PageSection>
  )
}
