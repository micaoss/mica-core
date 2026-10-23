import { useEffect, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { DnsDocument, DnsProbe, DnsTestResult, StaticRecord, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DataTable } from '@/shared/components/data-table'
import { FactList } from '@/shared/components/fact-list'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { PageSection } from '@/shared/components/page'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail, notifyFailure, notifySuccess } from '@/shared/feedback/toast'
import { EditBar } from './change-bar'
import { isIPv4 } from './model'
import type { NetworkData } from './use-network'

const MAX_SERVERS = 3
const MAX_RECORDS = 100
const MAX_COMMENT = 64
const HOST_NAME = /^(?=.{1,253}$)[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/

interface GlobalDraft { enabled: boolean; servers: string[] }

/// Which servers the device resolves with, the global servers that replace
/// every interface's, and the names it resolves itself. Each interface's DNS
/// is set on its own page.
export function DnsTab({ data, onEditing }: { data: NetworkData; onEditing: (editing: boolean) => void }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const dns = useDns()
  const global = dns.data?.global ?? { enabled: false, servers: [] }
  const records = dns.data?.staticRecords ?? []
  const [draft, setDraft] = useState<GlobalDraft>()
  const [tests, setTests] = useState<Record<string, DnsTestResult>>()
  const [testing, setTesting] = useState(false)
  const [confirming, setConfirming] = useState(false)
  useEffect(() => { onEditing(draft !== undefined); return () => onEditing(false) }, [draft, onEditing])

  const own = data.applied.order.map((name) => data.applied.ifaces[name].obs?.address?.split('/')[0]).filter(Boolean)
  const errors: Record<number, string> = {}
  if (draft?.enabled) {
    if (!draft.servers[0]) errors[0] = t(draft.servers.some(Boolean) ? 'network.dns.firstMissing' : 'network.dns.noneWhileOn')
    draft.servers.forEach((server, index) => {
      if (!server) return
      if (!isIPv4(server)) errors[index] = t('network.dns.notIpv4')
      else if (server.startsWith('127.') || server === '0.0.0.0') errors[index] = t('network.dns.loopback')
      else if (own.includes(server)) errors[index] = t('network.dns.ownAddress')
      else if (draft.servers.indexOf(server) !== index) errors[index] = t('network.dns.duplicate')
    })
  }
  const servers = draft ? draft.servers.filter(Boolean) : []
  const dirty = draft !== undefined && (draft.enabled !== global.enabled || (draft.enabled && servers.join() !== global.servers.join()))
  const turningOn = draft !== undefined && draft.enabled && !global.enabled
  const turningOff = draft !== undefined && !draft.enabled && global.enabled
  const unreachable = servers.filter((server) => tests?.[server]?.reachable === false)

  const start = () => { setDraft({ enabled: global.enabled, servers: [0, 1, 2].map((index) => global.servers[index] ?? '') }); setTests(undefined) }
  const cancel = () => { setDraft(undefined); setTests(undefined) }
  const runTest = async () => {
    const list = (draft?.servers ?? []).filter((server) => isIPv4(server))
    if (!list.length) return
    setTesting(true)
    try {
      const results = await api<DnsTestResult[]>('/api/v1/dns/test', json('POST', { servers: list }))
      setTests(Object.fromEntries(results.map((result) => [result.server, result])))
    } catch (error) {
      notifyFailure(t('network.dns.test'), failureDetail(error, t('common.requestFailed')))
    } finally {
      setTesting(false)
    }
  }
  // The servers are kept while switched off, so switching back restores them.
  const saveGlobal = async () => {
    if (!draft) return
    const value = { enabled: draft.enabled, servers: draft.enabled ? servers : global.servers }
    try {
      await api<TaskAccepted>('/api/v1/dns/global', json('PUT', value))
      cancel()
    } finally {
      void queryClient.invalidateQueries({ queryKey: ['dns'] })
      void queryClient.invalidateQueries({ queryKey: ['observed-network'] })
    }
  }
  const saved = t(turningOn ? 'network.dns.turnedOn' : turningOff ? 'network.dns.turnedOff' : 'network.dns.globalSaved')
  const askOrSave = () => {
    if (turningOn || turningOff || (draft?.enabled && unreachable.length)) setConfirming(true)
    else void saveGlobal().then(() => notifySuccess(saved), (error: unknown) => notifyFailure(t('network.dns.saveFailed'), failureDetail(error, t('common.requestFailed'))))
  }
  // Each record is written on its own, by id, and takes effect at once.
  const writeRecord = async (record: Omit<StaticRecord, 'id'>, id?: string) => {
    try {
      await api<StaticRecord>(id ? `/api/v1/dns/static-records/${encodeURIComponent(id)}` : '/api/v1/dns/static-records', json(id ? 'PUT' : 'POST', record))
    } finally {
      void queryClient.invalidateQueries({ queryKey: ['dns'] })
    }
  }
  const removeRecord = async (id: string) => {
    try {
      await api<void>(`/api/v1/dns/static-records/${encodeURIComponent(id)}`, { method: 'DELETE' })
    } finally {
      void queryClient.invalidateQueries({ queryKey: ['dns'] })
    }
  }

  return (
    <>
      {dns.error ? <Callout tone="danger" title={failureDetail(dns.error, t('common.requestFailed'))} /> : null}
      <Effective data={data} dns={dns.data} />

      <PageSection title={t('network.dns.global')} description={draft ? t('network.wifi.editHint') : t('network.dns.globalCopy')}>
        <Panel className={draft ? 'ring-2 ring-primary/40' : undefined}>
          {draft ? (
            <div className="grid gap-5">
              <ToggleField
                title={t('network.dns.global')}
                description={t(draft.enabled ? 'network.dns.onHint' : 'network.dns.offHint')}
                control={<Switch checked={draft.enabled} onCheckedChange={(enabled) => setDraft({ ...draft, enabled })} aria-label={t('network.dns.global')} />}
              />
              {draft.enabled ? (
                <>
                  {[0, 1, 2].slice(0, MAX_SERVERS).map((index) => {
                    const result = tests?.[draft.servers[index]]
                    return (
                      <FormField key={index} label={t(`network.dns.slots.${index}`)} hint={errors[index] ? <span className="text-destructive">{errors[index]}</span> : index === 0 && !draft.servers.some(Boolean) ? t('network.dns.suggested') : undefined}>
                        {(id) => (
                          <div className="flex flex-wrap items-center gap-3">
                            <Input id={id} className="w-56 font-mono" value={draft.servers[index]} placeholder={t(index === 0 ? 'network.dns.required' : 'network.dns.optional')} aria-invalid={Boolean(errors[index])} onChange={(event) => { const next = [...draft.servers]; next[index] = event.target.value.trim(); setDraft({ ...draft, servers: next }); setTests(undefined) }} />
                            {result ? (result.reachable
                              ? <span className="text-sm text-success">{t('network.dns.reachable', { ms: result.latencyMs })}</span>
                              : <span className="text-sm text-destructive">{t('network.dns.timedOut')}</span>) : null}
                          </div>
                        )}
                      </FormField>
                    )
                  })}
                  <FormField label={t('network.dns.test')} hint={t('network.dns.testHint', { name: dns.data?.probe?.name ?? '' })}>
                    {() => (
                      <div>
                        <Button size="sm" variant="outline" disabled={testing || !draft.servers.some((server) => isIPv4(server))} onClick={() => void runTest()}>
                          {testing ? <Spinner /> : null}{t(testing ? 'network.dns.testing' : 'network.dns.testServers')}
                        </Button>
                      </div>
                    )}
                  </FormField>
                </>
              ) : null}
            </div>
          ) : (
            <>
              <div><Button size="sm" onClick={start}>{t('network.detail.edit')}</Button></div>
              <FactList facts={[
                { id: 'state', label: t('network.fields.state'), value: global.enabled ? <span className="text-success">{t('network.values.on')}</span> : t('network.values.off') },
                global.enabled && { id: 'servers', label: t('network.dns.global'), value: <span className="grid justify-items-end gap-0.5">{global.servers.map((server, index) => <span key={server} className="font-mono">{t(`network.dns.slots.${index}`)}: {server}</span>)}</span> },
              ]} />
            </>
          )}
        </Panel>
      </PageSection>

      <StaticRecords records={records} hostname={data.hostname ?? ''} isPending={dns.isPending} write={writeRecord} remove={removeRecord} />

      {draft ? <EditBar what={t('network.dns.global')} dirty={dirty} errors={Object.keys(errors).length} onCancel={cancel} onDone={askOrSave} doneLabel={t('common.actions.save')} /> : null}
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        tone={turningOff ? 'danger' : 'neutral'}
        title={t(turningOff ? 'network.dns.confirmOff' : turningOn ? 'network.dns.confirmOn' : 'network.dns.confirmSave')}
        description={(
          <span className="grid gap-2">
            {draft?.enabled ? <span>{t('network.dns.onCopy')}</span> : null}
            <span>{t('network.dns.interruptCopy')}</span>
            {draft?.enabled && unreachable.length ? <span className="text-destructive">{t('network.dns.unreachableCopy', { list: unreachable.join(', ') })}</span> : null}
          </span>
        )}
        confirmLabel={t(turningOff ? 'network.dns.turnOff' : 'common.actions.save')}
        success={saved}
        failure={t('network.dns.saveFailed')}
        onConfirm={saveGlobal}
      />
    </>
  )
}

/// `GET /api/v1/dns`, which the overview reads too.
export function useDns() {
  return useQuery({ queryKey: ['dns'], queryFn: () => api<DnsDocument>('/api/v1/dns'), refetchInterval: 10_000 })
}

/// The links whose servers are worth showing: the interfaces the pages list.
export function dnsLinks(data: NetworkData, dns: DnsDocument | undefined) {
  return (dns?.effective.links ?? []).filter((link) => data.applied.ifaces[link.name] && data.applied.ifaces[link.name].kind !== 'can')
}

export function ProbeText({ probe }: { probe: DnsProbe | null | undefined }) {
  const { t } = useTranslation()
  if (!probe) return <span>—</span>
  return <span className={probe.ok ? 'text-success' : 'text-destructive'}>{t(probe.ok ? 'network.dns.resolves' : 'network.dns.unreachable')}{probe.ok && probe.latencyMs !== null ? ` · ${probe.latencyMs} ms` : ''}</span>
}

function Effective({ data, dns }: { data: NetworkData; dns: DnsDocument | undefined }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [checking, setChecking] = useState(false)
  const global = dns?.mode === 'global'
  const links = dnsLinks(data, dns)
  const probe = dns?.probe
  const recheck = async () => {
    setChecking(true)
    try {
      await api<DnsProbe>('/api/v1/dns/probe', json('POST', {}))
    } catch (error) {
      notifyFailure(t('network.dns.recheck'), failureDetail(error, t('common.requestFailed')))
    } finally {
      setChecking(false)
      void queryClient.invalidateQueries({ queryKey: ['dns'] })
    }
  }
  return (
    <PageSection title={t('network.dns.effective')} description={t('network.dns.effectiveCopy')}>
      <Panel>
        <FactList facts={[
          {
            id: 'mode',
            label: t('network.dns.mode'),
            value: global
              ? <span><strong>{t('network.dns.globalMode')}</strong> <span className="text-muted-foreground">· {t('network.dns.globalModeCopy')}</span></span>
              : <span><strong>{t('network.dns.perInterface')}</strong> <span className="text-muted-foreground">· {t('network.dns.perInterfaceCopy')}</span></span>,
          },
          {
            id: 'servers',
            label: t('network.dns.servers'),
            value: global ? (
              <span className="grid justify-items-end gap-0.5">{(dns?.effective.global ?? []).map((server, index) => <span key={server} className="font-mono">{index + 1}. {server}</span>)}</span>
            ) : links.length ? (
              <span className="grid justify-items-start gap-0.5 text-left">
                {links.map((link) => (
                  <span key={link.name}>
                    <Link className="font-mono text-primary" to="/network/$name" params={{ name: link.name }}>{link.name}</Link>
                    : <span className="font-mono">{link.servers.length ? link.servers.join(', ') : t('network.values.none')}</span> <span className="text-muted-foreground">({t(`network.dns.sources.${link.source}`)})</span>
                  </span>
                ))}
              </span>
            ) : <span className="text-muted-foreground">{t('network.values.none')}</span>,
          },
          { id: 'records', label: t('network.dns.records'), value: t('network.dns.recordCount', { count: dns?.staticRecords.length ?? 0 }) },
          {
            id: 'probe',
            label: t('network.dns.probe'),
            value: (
              <span className="flex flex-wrap items-center justify-end gap-2">
                <ProbeText probe={probe} />
                {probe ? <span className="font-mono text-muted-foreground">{probe.name}</span> : null}
                <Button size="sm" variant="outline" disabled={checking} onClick={() => void recheck()}>
                  {checking ? <Spinner /> : null}{t('network.dns.recheck')}
                </Button>
              </span>
            ),
          },
        ]} />
      </Panel>
    </PageSection>
  )
}

/// The names the device resolves itself. Each record is written on its own
/// and takes effect without a network reload.
function StaticRecords({ records, hostname, isPending, write, remove }: { records: StaticRecord[]; hostname: string; isPending: boolean; write: (record: Omit<StaticRecord, 'id'>, id?: string) => Promise<void>; remove: (id: string) => Promise<void> }) {
  const { t } = useTranslation()
  const [query, setQuery] = useState('')
  const [editing, setEditing] = useState<{ index?: number; record: Omit<StaticRecord, 'id'> & { id?: string } }>()
  const shown = records.map((record, index) => ({ record, index })).filter(({ record }) => !query || record.hostname.includes(query.trim().toLowerCase()))
  const record = editing?.record
  const name = record?.hostname.trim().toLowerCase() ?? ''
  const errors: Record<string, string> = {}
  if (record) {
    if (!name) errors.hostname = t('network.dns.hostnameMissing')
    else if (name.includes('*')) errors.hostname = t('network.dns.wildcard')
    else if (!HOST_NAME.test(name)) errors.hostname = t('network.dns.hostnameInvalid')
    else if (name === 'localhost' || name === hostname.toLowerCase()) errors.hostname = t('network.dns.hostnameReserved')
    else if (records.some((each, index) => index !== editing?.index && each.hostname.toLowerCase() === name)) errors.hostname = t('network.dns.hostnameTaken')
    if (!isIPv4(record.address)) errors.address = t('network.dns.notIpv4')
    if ((record.comment ?? '').length > MAX_COMMENT) errors.comment = t('network.dns.commentLong', { count: MAX_COMMENT })
  }
  const submit = () => {
    if (!editing || !record) return Promise.resolve()
    const comment = record.comment?.trim()
    return write({ hostname: name, address: record.address, ...(comment ? { comment } : {}) }, record.id)
  }
  const hint = (key: string, fallback?: string) => errors[key] && (key !== 'hostname' || record?.hostname) ? <span className="text-destructive">{errors[key]}</span> : fallback

  return (
    <PageSection title={t('network.dns.records')} description={t('network.dns.recordsCopy')}>
      <CollectionPanel
        title={t('network.dns.records')}
        action={(
          <span className="flex flex-wrap items-center gap-2">
            <Input className="h-8 w-44" value={query} placeholder={t('network.dns.search')} aria-label={t('network.dns.search')} onChange={(event) => setQuery(event.target.value)} />
            <Button size="sm" variant="outline" disabled={records.length >= MAX_RECORDS} onClick={() => setEditing({ record: { hostname: '', address: '' } })}><Plus />{t('network.dns.addRecord')}</Button>
          </span>
        )}
        footer={<span className="text-xs text-muted-foreground">{t('network.dns.recordLimit', { count: records.length, max: MAX_RECORDS })}</span>}
      >
        <DataTable<{ record: StaticRecord; index: number }>
          rows={shown}
          rowKey={({ record }) => record.id}
          isPending={isPending}
          empty={t(records.length ? 'network.dns.noMatch' : 'network.dns.noRecords')}
          columns={[
            { id: 'hostname', header: t('network.dns.hostname'), cell: ({ record }) => <strong className="font-mono">{record.hostname}</strong> },
            { id: 'address', header: t('network.dns.address'), cell: ({ record }) => <span className="font-mono">{record.address}</span> },
            { id: 'comment', header: t('network.dns.comment'), cell: ({ record }) => <span className="text-muted-foreground">{record.comment || '—'}</span> },
            { id: 'actions', header: '', align: 'end', cell: ({ record, index }) => (
              <span className="flex justify-end gap-2">
                <Button type="button" size="sm" variant="outline" onClick={() => setEditing({ index, record: { ...record } })}>{t('common.actions.edit')}</Button>
                <ConfirmDialog
                  trigger={<Button type="button" size="sm" variant="destructive" aria-label={t('network.dns.removeNamed', { name: record.hostname })}><Trash2 /></Button>}
                  title={t('network.dns.removeNamed', { name: record.hostname })}
                  description={t('network.dns.removeCopy')}
                  confirmLabel={t('network.dns.remove')}
                  tone="danger"
                  success={t('network.dns.removed', { name: record.hostname })}
                  failure={t('network.dns.remove')}
                  onConfirm={() => remove(record.id)}
                />
              </span>
            ) },
          ]}
        />
      </CollectionPanel>
      <FormDialog
        open={editing !== undefined}
        onOpenChange={(next) => { if (!next) setEditing(undefined) }}
        title={t(editing?.index === undefined ? 'network.dns.addRecord' : 'network.dns.editRecord')}
        description={t('network.dns.recordDialogCopy')}
        submitLabel={t('common.actions.save')}
        success={t('network.dns.recordSaved', { name })}
        failure={t('network.dns.saveFailed')}
        onSubmit={submit}
        submitDisabled={Object.keys(errors).length > 0}
      >
        <FormField label={t('network.dns.hostname')} hint={hint('hostname', /\.local$/i.test(name) ? t('network.dns.mdnsHint') : t('network.dns.hostnameHint'))}>
          {(id) => <Input id={id} className="font-mono" value={record?.hostname ?? ''} aria-invalid={Boolean(errors.hostname && record?.hostname)} onChange={(event) => record && setEditing({ ...editing, record: { ...record, hostname: event.target.value } })} />}
        </FormField>
        <FormField label={t('network.dns.address')} hint={record?.address ? hint('address') : undefined}>
          {(id) => <Input id={id} className="font-mono" value={record?.address ?? ''} placeholder="192.168.1.20" aria-invalid={Boolean(errors.address && record?.address)} onChange={(event) => record && setEditing({ ...editing, record: { ...record, address: event.target.value.trim() } })} />}
        </FormField>
        <FormField label={t('network.dns.comment')} hint={hint('comment', t('network.dns.optional'))}>
          {(id) => <Input id={id} value={record?.comment ?? ''} aria-invalid={Boolean(errors.comment)} onChange={(event) => record && setEditing({ ...editing, record: { ...record, comment: event.target.value } })} />}
        </FormField>
      </FormDialog>
    </PageSection>
  )
}
