import { useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Package } from 'lucide-react'
import { ApiError, api } from '@/shared/lib/http'
import type { AppEntry, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FilePicker } from '@/shared/components/file-picker'
import { CollectionPanel } from '@/shared/components/panel'
import { StatusBadge } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { APPS, appState, appTone, useApps } from './apps'
import { PrivilegesList, type PrivilegesAsked } from './privileges-asked'

/// The bundle is streamed as its own bytes, as an update archive is.
function postBundle(file: File, acceptPrivileges: boolean) {
  return api<TaskAccepted>(acceptPrivileges ? `${APPS}?acceptPrivileges=true` : APPS, {
    method: 'POST',
    headers: { 'content-type': 'application/octet-stream' },
    body: file,
  })
}

/// The user's apps: each bundle micad installed, and the upload of another.
/// A bundle asking for privileges the installed version lacks is installed
/// only once the user accepts them.
export function AppsPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const apps = useApps()
  const [task, setTask] = useState<string>()
  const [asked, setAsked] = useState<{ file: File, privileges: PrivilegesAsked }>()
  const taken = (accepted: TaskAccepted) => {
    setTask(accepted.taskId)
    void queryClient.invalidateQueries({ queryKey: ['apps'] })
  }
  const upload = useMutationFeedback<TaskAccepted, File>({
    mutationFn: (file) => postBundle(file, false),
    success: t('services.apps.uploaded'),
    failure: t('services.apps.upload'),
    onSuccess: taken,
    onError: (error, file) => {
      if (error instanceof ApiError && error.code === 'app_privileges') {
        setAsked({ file, privileges: (error.params ?? {}) as PrivilegesAsked })
      }
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
            {asked ? (
              <ConfirmDialog
                open
                onOpenChange={(open) => { if (!open) setAsked(undefined) }}
                tone={asked.privileges.root ? 'danger' : 'neutral'}
                title={t('services.apps.privileges.title')}
                description={(
                  <span className="grid gap-2">
                    <span>{t('services.apps.privileges.copy')}</span>
                    <PrivilegesList asked={asked.privileges} />
                  </span>
                )}
                confirmLabel={t('services.apps.privileges.accept')}
                success={t('services.apps.uploaded')}
                failure={t('services.apps.upload')}
                onConfirm={async () => {
                  taken(await postBundle(asked.file, true))
                  setAsked(undefined)
                }}
              />
            ) : null}
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
