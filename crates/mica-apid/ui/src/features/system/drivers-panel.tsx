import { useCallback, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { TaskAccepted } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { formatDeviceClock } from '@/shared/components/fact'
import { FileChooser } from '@/shared/components/file-picker'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField } from '@/shared/components/form-field'
import { Input } from '@/shared/components/ui/input'
import { PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { RowItem, RowList } from '@/shared/components/row-item'
import { StatusBadge } from '@/shared/components/status-badge'
import type { StatusTone } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/shared/components/ui/tabs'
import { failureDetail } from '@/shared/feedback/toast'

type CertState = 'loaded' | 'rejected' | 'pending-next-boot' | 'pending-confirmation' | 'removed-next-boot'

/// One of `GET /api/v1/system/owner-certs`.
interface OwnerCert {
  sha256: string
  state: CertState
  reason?: string
  subject?: string
  issuer?: string
  notBefore?: number
  notAfter?: number
  key?: string
}

/// Kernel component id → extension name → extension id.
type Builds = Record<string, Record<string, string>>

/// `GET /api/v1/system/extensions`.
interface Extensions {
  kernel?: string
  installed?: Builds
  pending?: Builds | null
  pair?: string | null
  attemptsLeft?: number | null
  pendingVerification?: string[]
  waitsForExtension?: { name: string; kernel: string }[]
  boot?: {
    skipped?: string
    extensions: { name: string; id: string; version?: string; state: 'mapped' | 'unused' | 'refused'; reason?: string }[]
  } | null
}

const CERT_TONES: Record<CertState, StatusTone> = {
  loaded: 'success',
  rejected: 'danger',
  'pending-next-boot': 'accent',
  'pending-confirmation': 'warning',
  'removed-next-boot': 'neutral',
}

const BOOT_TONES = { mapped: 'success', unused: 'warning', refused: 'danger' } as const

/// The bytes of `file` in base64, as the certificate route takes them.
async function base64(file: File) {
  const bytes = new Uint8Array(await file.arrayBuffer())
  let binary = ''
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary)
}

/// The owner certificates and driver extensions, each change asking for the
/// administrator password and applying from the next boot.
export function DriversPanel() {
  const { t } = useTranslation()
  return (
    <>
      <PageSection title={t('system.drivers.certs.title')} description={t('system.drivers.certs.description')}><OwnerCertsPanel /></PageSection>
      <PageSection title={t('system.drivers.extensions.title')} description={t('system.drivers.extensions.description')}><ExtensionsPanel /></PageSection>
    </>
  )
}

/// A change the password confirms: the dialog, the task it answers and the
/// read it refreshes once the task finishes.
function useConfirmedChange(queryKey: string) {
  const queryClient = useQueryClient()
  const [taskId, setTaskId] = useState<string>()
  const [password, setPassword] = useState('')
  const finished = useCallback(() => void queryClient.invalidateQueries({ queryKey: [queryKey] }), [queryClient, queryKey])
  const change = useMutation({
    mutationFn: (request: () => Promise<TaskAccepted>) => request(),
    onSuccess: (accepted) => {
      setTaskId(accepted.taskId)
      setPassword('')
    },
  })
  return { taskId, password, setPassword, finished, run: change.mutateAsync }
}

function PasswordField({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const { t } = useTranslation()
  return (
    <FormField label={t('system.drivers.password')}>
      {(id) => <Input id={id} type="password" autoComplete="current-password" value={value} onChange={(event) => onChange(event.target.value)} required />}
    </FormField>
  )
}

function FileField({ label, accept, onChange }: { label: string; accept?: string; onChange: (file?: File) => void }) {
  const { t } = useTranslation()
  return <FileChooser label={label} accept={accept} chooseLabel={t('system.drivers.choose')} emptyLabel={t('system.drivers.noFile')} onChange={onChange} />
}

function OwnerCertsPanel() {
  const { t, i18n } = useTranslation()
  const certs = useQuery({ queryKey: ['owner-certs'], queryFn: () => api<OwnerCert[]>('/api/v1/system/owner-certs') })
  const change = useConfirmedChange('owner-certs')
  const [adding, setAdding] = useState(false)
  const [file, setFile] = useState<File>()
  const [removing, setRemoving] = useState<OwnerCert>()
  const locale = i18n.resolvedLanguage ?? 'en'
  return (
    <Panel>
      <Callout title={t('system.drivers.nextBoot')} />
      {certs.error ? <Callout tone="danger" title={failureDetail(certs.error, t('common.requestFailed'))} /> : null}
      {certs.data?.length === 0 ? <p className="text-sm text-muted-foreground">{t('system.drivers.certs.none')}</p> : null}
      {certs.data?.length ? (
        <RowList>
          {certs.data.map((cert) => (
            <RowItem
              key={cert.sha256}
              title={cert.subject ?? cert.sha256}
              description={[
                cert.key,
                cert.notAfter === undefined ? undefined : t('system.drivers.certs.validUntil', { time: formatDeviceClock(cert.notAfter, locale) }),
                `SHA-256 ${cert.sha256.slice(0, 16)}…`,
                cert.reason,
              ].filter(Boolean).join(' · ')}
              actions={(
                <>
                  <StatusBadge tone={CERT_TONES[cert.state]}>{t(`system.drivers.certs.states.${cert.state}`, { defaultValue: cert.state })}</StatusBadge>
                  {cert.state === 'removed-next-boot' ? null : (
                    <Button size="sm" variant="outline" onClick={() => setRemoving(cert)}>{t('system.drivers.remove')}</Button>
                  )}
                </>
              )}
            />
          ))}
        </RowList>
      ) : null}
      <Button className="justify-self-start" onClick={() => setAdding(true)}>{t('system.drivers.certs.add')}</Button>
      <TaskProgress taskId={change.taskId} onFinished={change.finished} />
      <FormDialog
        open={adding}
        onOpenChange={setAdding}
        title={t('system.drivers.certs.addTitle')}
        description={t('system.drivers.certs.addCopy')}
        submitLabel={t('system.drivers.certs.addSubmit')}
        success={t('system.drivers.queued')}
        failure={t('system.drivers.certs.add')}
        submitDisabled={!file}
        onSubmit={() => change.run(async () => api<TaskAccepted>('/api/v1/system/owner-certs', json('POST', { password: change.password, certificate: await base64(file!) })))}
      >
        <FileField label={t('system.drivers.certs.file')} accept=".pem,.crt,.cer,.der" onChange={setFile} />
        <PasswordField value={change.password} onChange={change.setPassword} />
      </FormDialog>
      <FormDialog
        open={removing !== undefined}
        onOpenChange={(open) => { if (!open) setRemoving(undefined) }}
        title={t('system.drivers.certs.removeTitle')}
        description={t('system.drivers.certs.removeCopy', { subject: removing?.subject ?? removing?.sha256 })}
        submitLabel={t('system.drivers.remove')}
        success={t('system.drivers.queued')}
        failure={t('system.drivers.remove')}
        onSubmit={() => change.run(() => api<TaskAccepted>(`/api/v1/system/owner-certs/${removing!.sha256}`, json('DELETE', { password: change.password })))}
      >
        <PasswordField value={change.password} onChange={change.setPassword} />
      </FormDialog>
    </Panel>
  )
}

/// One extension of the running kernel as the console shows it.
interface ExtensionRow { name: string; id?: string; version?: string; tone: StatusTone; state: string; reason?: string }

function extensionRows(doc: Extensions, t: (key: string) => string): ExtensionRow[] {
  const kernel = doc.kernel ?? ''
  const installed = doc.installed?.[kernel] ?? {}
  // A change alone on trial is what the running kernel's builds become; a
  // pair's is the new system's.
  const target = doc.pending && !doc.pair ? doc.pending[kernel] ?? {} : installed
  const booted = doc.boot?.extensions ?? []
  const names = new Set([...Object.keys(installed), ...Object.keys(target), ...booted.map((entry) => entry.name)])
  return [...names].sort().map((name) => {
    const id = target[name]
    const boot = booted.find((entry) => entry.name === name)
    if (id === undefined) return { name, id: boot?.id, version: boot?.version, tone: 'neutral', state: t('system.drivers.extensions.states.removedNextBoot') }
    if (doc.pendingVerification?.includes(id) || boot?.id !== id) {
      return { name, id, version: boot?.id === id ? boot.version : undefined, tone: 'accent', state: t('system.drivers.extensions.states.pendingVerification') }
    }
    return { name, id, version: boot.version, tone: BOOT_TONES[boot.state], state: t(`system.drivers.extensions.states.${boot.state}`), reason: boot.reason }
  })
}

function ExtensionsPanel() {
  const { t } = useTranslation()
  const extensions = useQuery({ queryKey: ['extensions'], queryFn: () => api<Extensions>('/api/v1/system/extensions') })
  const change = useConfirmedChange('extensions')
  const [uploading, setUploading] = useState(false)
  const [mode, setMode] = useState<'archive' | 'files'>('archive')
  const [files, setFiles] = useState<Partial<Record<'archive' | 'descriptor' | 'image' | 'signature', File>>>({})
  const [removing, setRemoving] = useState<string>()
  const [dropping, setDropping] = useState<{ name: string; kernel: string }>()
  const doc = extensions.data
  const rows = doc ? extensionRows(doc, t) : []
  const needed = mode === 'archive' ? ['archive'] as const : ['descriptor', 'image', 'signature'] as const
  const upload = () => {
    const form = new FormData()
    form.append('password', change.password)
    for (const field of needed) form.append(field, files[field]!)
    return change.run(() => api<TaskAccepted>('/api/v1/system/extensions', { method: 'POST', body: form }))
  }
  return (
    <Panel>
      <Callout title={t('system.drivers.nextBoot')} />
      {extensions.error ? <Callout tone="danger" title={failureDetail(extensions.error, t('common.requestFailed'))} /> : null}
      {doc?.boot?.skipped ? <Callout tone="warning" title={t('system.drivers.extensions.skipped', { reason: doc.boot.skipped })} /> : null}
      {doc?.pending ? (
        <Callout tone="warning" title={doc.pair ? t('system.drivers.extensions.pendingPair') : t('system.drivers.extensions.pendingTrial', { count: doc.attemptsLeft ?? 0 })} />
      ) : null}
      {doc?.waitsForExtension?.length ? (
        <Callout tone="warning" title={t('system.drivers.extensions.waitsTitle')}>
          <p>{t('system.drivers.extensions.waitsCopy')}</p>
          <RowList>
            {doc.waitsForExtension.map((wait) => (
              <RowItem
                key={`${wait.kernel}/${wait.name}`}
                title={wait.name}
                description={t('system.drivers.extensions.forKernel', { kernel: wait.kernel.slice(0, 12) })}
                actions={(
                  <>
                    <Button size="sm" onClick={() => setUploading(true)}>{t('system.drivers.extensions.uploadBuild')}</Button>
                    <Button size="sm" variant="outline" onClick={() => setDropping(wait)}>{t('system.drivers.extensions.drop')}</Button>
                  </>
                )}
              />
            ))}
          </RowList>
        </Callout>
      ) : null}
      {doc && rows.length === 0 ? <p className="text-sm text-muted-foreground">{t('system.drivers.extensions.none')}</p> : null}
      {rows.length ? (
        <RowList>
          {rows.map((row) => (
            <RowItem
              key={row.name}
              title={row.version ? `${row.name} ${row.version}` : row.name}
              description={row.reason}
              actions={(
                <>
                  <StatusBadge tone={row.tone}>{row.state}</StatusBadge>
                  {row.tone === 'neutral' ? null : <Button size="sm" variant="outline" onClick={() => setRemoving(row.name)}>{t('system.drivers.remove')}</Button>}
                </>
              )}
            />
          ))}
        </RowList>
      ) : null}
      <Button className="justify-self-start" onClick={() => setUploading(true)}>{t('system.drivers.extensions.upload')}</Button>
      <TaskProgress taskId={change.taskId} onFinished={change.finished} />
      <FormDialog
        open={uploading}
        onOpenChange={setUploading}
        title={t('system.drivers.extensions.uploadTitle')}
        description={t('system.drivers.extensions.uploadCopy')}
        submitLabel={t('system.drivers.extensions.uploadSubmit')}
        success={t('system.drivers.queued')}
        failure={t('system.drivers.extensions.upload')}
        submitDisabled={needed.some((field) => !files[field])}
        onSubmit={upload}
      >
        <Tabs value={mode} onValueChange={(value) => setMode(value as 'archive' | 'files')}>
          <TabsList aria-label={t('system.drivers.extensions.uploadTitle')}>
            <TabsTrigger value="archive">{t('system.drivers.extensions.modes.archive')}</TabsTrigger>
            <TabsTrigger value="files">{t('system.drivers.extensions.modes.files')}</TabsTrigger>
          </TabsList>
          <TabsContent value="archive" className="grid gap-4 pt-4">
            <FileField label={t('system.drivers.extensions.files.archive')} accept=".micaupd" onChange={(file) => setFiles({ ...files, archive: file })} />
          </TabsContent>
          <TabsContent value="files" className="grid gap-4 pt-4">
            <FileField label={t('system.drivers.extensions.files.descriptor')} accept=".json" onChange={(file) => setFiles({ ...files, descriptor: file })} />
            <FileField label={t('system.drivers.extensions.files.image')} accept=".img" onChange={(file) => setFiles({ ...files, image: file })} />
            <FileField label={t('system.drivers.extensions.files.signature')} accept=".p7s" onChange={(file) => setFiles({ ...files, signature: file })} />
          </TabsContent>
        </Tabs>
        <PasswordField value={change.password} onChange={change.setPassword} />
      </FormDialog>
      <FormDialog
        open={removing !== undefined}
        onOpenChange={(open) => { if (!open) setRemoving(undefined) }}
        title={t('system.drivers.extensions.removeTitle')}
        description={t('system.drivers.extensions.removeCopy', { name: removing })}
        submitLabel={t('system.drivers.remove')}
        success={t('system.drivers.queued')}
        failure={t('system.drivers.remove')}
        onSubmit={() => change.run(() => api<TaskAccepted>(`/api/v1/system/extensions/${removing!}`, json('DELETE', { password: change.password })))}
      >
        <PasswordField value={change.password} onChange={change.setPassword} />
      </FormDialog>
      <FormDialog
        open={dropping !== undefined}
        onOpenChange={(open) => { if (!open) setDropping(undefined) }}
        title={t('system.drivers.extensions.dropTitle')}
        description={t('system.drivers.extensions.dropCopy', { name: dropping?.name })}
        submitLabel={t('system.drivers.extensions.drop')}
        success={t('system.drivers.queued')}
        failure={t('system.drivers.extensions.drop')}
        onSubmit={() => change.run(() => api<TaskAccepted>(`/api/v1/system/extensions/${dropping!.name}/drop`, json('POST', { password: change.password, kernel: dropping!.kernel })))}
      >
        <PasswordField value={change.password} onChange={change.setPassword} />
      </FormDialog>
    </Panel>
  )
}
