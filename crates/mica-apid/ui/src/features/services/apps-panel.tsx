import { useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Package } from 'lucide-react'
import { ApiError, api } from '@/shared/lib/http'
import type { AppEntry, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { DataTable } from '@/shared/components/data-table'
import { FilePicker } from '@/shared/components/file-picker'
import { CollectionPanel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { APPS, appState, appTone, useApps } from './apps'

/// The user's apps: each bundle micad installed, and the upload of another.
export function AppsPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const apps = useApps()
  const [task, setTask] = useState<string>()
  // The bundle is streamed as its own bytes, as an update archive is.
  const upload = useMutationFeedback<TaskAccepted, File>({
    mutationFn: (file) => api<TaskAccepted>(APPS, {
      method: 'POST',
      headers: { 'content-type': 'application/octet-stream' },
      body: file,
    }),
    success: t('services.apps.uploaded'),
    failure: t('services.apps.upload'),
    onSuccess: (accepted) => {
      setTask(accepted.taskId)
      void queryClient.invalidateQueries({ queryKey: ['apps'] })
    },
  })
  const rules = upload.error instanceof ApiError ? upload.error.rules ?? [] : []

  return (
    <div className="grid gap-3">
      {apps.error ? <Callout tone="danger" title={failureDetail(apps.error, t('common.requestFailed'))} /> : null}
      <CollectionPanel
        title={t('services.apps.title')}
        description={t('services.apps.copy')}
        footer={apps.data?.available === false ? undefined : (
          <div className="grid gap-3">
            <FilePicker
              label={t('services.apps.bundle')}
              hint={t('services.apps.bundleHint', { arch: apps.data?.arch ?? '' })}
              accept=".tar.gz,.tgz,application/gzip"
              chooseLabel={t('services.apps.choose')}
              emptyLabel={t('services.apps.noBundle')}
              submitLabel={t('services.apps.upload')}
              pendingLabel={t('services.apps.uploading')}
              pending={upload.isPending}
              onSubmit={(file) => { setTask(undefined); upload.mutate(file) }}
            />
            {rules.length > 0 ? (
              <Callout tone="danger" title={t('services.apps.refused')}>
                <ul className="list-disc pl-5">
                  {rules.map((rule) => <li key={`${rule.rule}:${rule.detail}`}><span className="font-mono">{rule.rule}</span>: {rule.detail}</li>)}
                </ul>
              </Callout>
            ) : null}
            <TaskProgress taskId={task} />
          </div>
        )}
      >
        {apps.data?.available === false ? (
          <p className="p-4 text-sm text-muted-foreground">{t('services.apps.unavailable')}</p>
        ) : (
          <DataTable<AppEntry>
            rows={apps.data?.apps}
            rowKey={(row) => row.name}
            isPending={apps.isPending}
            empty={t('services.apps.empty')}
            emptyIcon={<Package />}
            columns={[
              { id: 'name', header: t('services.apps.name'), cell: (row) => (
                <span className="flex flex-col">
                  <span className="flex flex-wrap items-center gap-2">
                    <Link to="/services/apps/$name" params={{ name: row.name }} className="font-mono font-medium text-primary hover:underline">{row.name}</Link>
                    {row.privileges?.user === 'root' ? <StatusBadge tone="warning">{t('services.apps.runsAsRoot')}</StatusBadge> : null}
                  </span>
                  {row.description ? <small className="text-muted-foreground">{row.description}</small> : null}
                </span>
              ) },
              { id: 'version', header: t('services.apps.version'), cell: (row) => (
                <span className="flex flex-wrap items-center gap-2">
                  <span className="font-mono">{row.version}</span>
                  {row.update?.available ? <StatusBadge tone="accent">{t('services.apps.available', { version: row.update.available })}</StatusBadge> : null}
                </span>
              ) },
              { id: 'state', header: t('services.apps.state'), align: 'end', cell: (row) => (
                <StatusBadge tone={appTone(row)}>{t(`services.apps.states.${appState(row)}`, { defaultValue: appState(row) })}</StatusBadge>
              ) },
            ]}
          />
        )}
      </CollectionPanel>
    </div>
  )
}
