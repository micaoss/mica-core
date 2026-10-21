import { useEffect, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ChevronLeft, KeyRound, Plus, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import { cn } from '@/shared/lib/utils'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { CopyField } from '@/shared/components/copy-field'
import { DataTable } from '@/shared/components/data-table'
import { FactList } from '@/shared/components/fact-list'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField } from '@/shared/components/form-field'
import { PageSection } from '@/shared/components/page'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { failureDetail, notifySuccess } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { EditBar } from './change-bar'
import { updateChangeSet } from './change-set'
import { CreateWizard } from './create-wizard'
import { overlaps, parseCidr, stageSet, type WireguardPeer } from './model'
import { Led, StatusLine, Tag, TEXT, useStatusText } from './parts'
import type { NetworkData } from './use-network'
import { computeStatus } from './view'

const KEY_PATTERN = /^[A-Za-z0-9+/]{43}=$/
const MAX_PEER_NAME = 64
const EMPTY_PEER = { name: '', publicKey: '', presharedKey: '', allowedIps: '', endpoint: '', keepalive: '' }
const shortKey = (key: string) => `${key.slice(0, 8)}…${key.slice(-6)}`

/// `10.10.0.2/24` as the route to that one host, `10.10.0.2/32`.
export function hostRoute(address: string | undefined) {
  if (!address) return ''
  const host = address.split('/')[0]
  return `${host}/${host.includes(':') ? 128 : 32}`
}

/// The device's tunnels, one card each; a card opens the tunnel.
export function WireguardTab({ data, tunnel, onEditing }: { data: NetworkData; tunnel?: string; onEditing: (editing: boolean) => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const text = useStatusText()
  const [adding, setAdding] = useState(false)
  const state = useQuery({ queryKey: ['state', 'network'], queryFn: () => api<Record<string, { publicKey?: string }>>('/api/v1/state/network'), retry: false })
  const applied = data.applied.order.filter((name) => data.applied.ifaces[name].kind === 'wireguard')
  const pendingOnly = data.staged.order.filter((name) => data.staged.ifaces[name].kind === 'wireguard' && !data.applied.ifaces[name])

  if (tunnel && applied.includes(tunnel)) return <TunnelDetail data={data} name={tunnel} publicKey={state.data?.[tunnel]?.publicKey} onEditing={onEditing} />

  const all = [...applied, ...pendingOnly]
  return (
    <>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="text-sm text-muted-foreground">{t('network.list.count', { count: all.length })}</span>
        <Button size="sm" onClick={() => setAdding(true)}><Plus />{t('network.list.add')}</Button>
      </div>
      {all.length ? (
        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
          {all.map((name) => {
            const isApplied = data.applied.ifaces[name] !== undefined
            const entry = (isApplied ? data.applied : data.staged).ifaces[name].entry
            const status = computeStatus(data.applied, name)
            const key = state.data?.[name]?.publicKey
            const open = () => void navigate({ to: '/network', search: { tab: 'wireguard', tunnel: name } })
            return (
              <div
                key={name}
                {...(isApplied ? { role: 'link', tabIndex: 0, 'aria-label': name, onClick: open, onKeyDown: (event: React.KeyboardEvent) => { if (event.key === 'Enter') open() } } : { title: t('network.wireguard.afterApply') })}
                className={cn('grid gap-2 rounded-xl border bg-card p-4', isApplied ? 'cursor-pointer hover:border-primary/50' : 'border-warning/60')}
              >
                <div className="flex items-center gap-2">
                  <Led tone={status.tone} /><span className="font-mono font-semibold">{name}</span><span className="text-xs text-muted-foreground">WireGuard</span>
                  <span className="flex-1" />
                  {!isApplied ? <Tag tone="pending">{t('network.tags.pendingCreate')}</Tag> : null}
                </div>
                <span className="font-mono text-sm">{entry.static?.address || '—'}</span>
                <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
                  <dt className="text-muted-foreground">{t('network.fields.state')}</dt><dd className={TEXT[status.tone]}>{text.label(status)}</dd>
                  <dt className="text-muted-foreground">{t('network.fields.listenPort')}</dt><dd className="font-mono">{entry.wireguard?.listenPort ?? t('network.values.automatic')}</dd>
                  <dt className="text-muted-foreground">{t('network.wireguard.peers')}</dt><dd>{isApplied ? t('network.wireguard.peerCount', { count: entry.wireguard?.peers.length ?? 0 }) : t('network.wireguard.afterApply')}</dd>
                  <dt className="text-muted-foreground">{t('network.wireguard.publicKey')}</dt><dd className="font-mono">{key ? shortKey(key) : <span className="text-muted-foreground">{t('network.wireguard.keyAfterApply')}</span>}</dd>
                </dl>
              </div>
            )
          })}
        </div>
      ) : (
        <Panel><p className="flex items-center justify-center gap-2 py-6 text-sm text-muted-foreground"><KeyRound className="size-4" />{t('network.wireguard.empty')}</p></Panel>
      )}
      {adding ? <CreateWizard data={data} kinds={['wireguard']} onClose={() => setAdding(false)} /> : null}
    </>
  )
}

function TunnelDetail({ data, name, publicKey, onEditing }: { data: NetworkData; name: string; publicKey?: string; onEditing: (editing: boolean) => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const staged = data.staged.ifaces[name]?.entry ?? data.applied.ifaces[name].entry
  const applied = data.applied.ifaces[name].entry
  const status = computeStatus(data.applied, name)
  const [draft, setDraft] = useState<{ address: string; port: string }>()
  useEffect(() => { onEditing(draft !== undefined); return () => onEditing(false) }, [draft, onEditing])
  const [adding, setAdding] = useState(false)
  const [peer, setPeer] = useState(EMPTY_PEER)
  const peers = useQuery({ queryKey: ['wireguard-peers', name], queryFn: () => api<WireguardPeer[]>(`/api/v1/network/${encodeURIComponent(name)}/peers`) })
  const invalidatePeers = () => {
    void queryClient.invalidateQueries({ queryKey: ['wireguard-peers', name] })
    void queryClient.invalidateQueries({ queryKey: ['network'] })
  }
  const remove = useMutationFeedback<void, string>({
    mutationFn: (key) => api<void>(`/api/v1/network/${encodeURIComponent(name)}/peers/${encodeURIComponent(key)}`, { method: 'DELETE' }),
    success: t('network.wireguard.removed'),
    failure: t('network.wireguard.remove'),
    onSuccess: invalidatePeers,
  })
  const rotate = useMutationFeedback<{ publicKey: string }>({
    mutationFn: () => api<{ publicKey: string }>(`/api/v1/actions/wireguard/${encodeURIComponent(name)}/rotate-key`, { method: 'POST' }),
    success: t('network.wireguard.rotatedKey'),
    failure: t('network.wireguard.rotate'),
  })
  // The rotation's answer first: it is newer than the live state last read.
  const key = rotate.data?.publicKey ?? publicKey
  const port = staged.wireguard?.listenPort ?? applied.wireguard?.listenPort
  const snippet = [
    '[Peer]',
    `# ${data.hostname ?? ''}`,
    `PublicKey = ${key ?? ''}`,
    ...(port ? [`Endpoint = ${data.localAddress ?? window.location.hostname}:${port}`] : []),
    `AllowedIPs = ${hostRoute(applied.static?.address)}`,
  ].join('\n')
  const tunnelPending = (staged.static?.address ?? '') !== (applied.static?.address ?? '') || staged.wireguard?.listenPort !== applied.wireguard?.listenPort

  const errors: { address?: string; port?: string } = {}
  if (draft) {
    const clash = data.staged.order.find((each) => each !== name && data.staged.ifaces[each].obs?.address && overlaps(draft.address, data.staged.ifaces[each].obs?.address ?? ''))
    if (!parseCidr(draft.address)) errors.address = t('network.problems.cidr')
    else if (clash) errors.address = t('network.problems.overlapWith', { name: clash })
    const listen = Number(draft.port)
    if (!(listen >= 1 && listen <= 65535)) errors.port = t('network.problems.portRange')
    else if (Object.entries(data.staged.map).some(([each, entry]) => each !== name && entry.kind === 'wireguard' && entry.wireguard?.listenPort === listen)) errors.port = t('network.problems.portTaken')
  }
  const saveTunnel = () => {
    if (!draft) return
    updateChangeSet((set) => stageSet(set, name, 'wireguard', (entry) => ({ ...entry, static: { ...entry.static, address: draft.address }, wireguard: { peers: entry.wireguard?.peers ?? [], ...entry.wireguard, listenPort: Number(draft.port) } })))
    setDraft(undefined)
    notifySuccess(t('network.edit.staged'))
  }
  const peerErrors: { name?: string; publicKey?: string; presharedKey?: string; allowedIps?: string; endpoint?: string; keepalive?: string } = {}
  if (peer.name.trim().length > MAX_PEER_NAME) peerErrors.name = t('network.wireguard.nameLong', { count: MAX_PEER_NAME })
  if (peer.presharedKey && !KEY_PATTERN.test(peer.presharedKey)) peerErrors.presharedKey = t('network.wireguard.keyFormat')
  const allowed = peer.allowedIps.split(/[,，\s]+/).map((item) => item.trim()).filter(Boolean)
  if (!KEY_PATTERN.test(peer.publicKey)) peerErrors.publicKey = t('network.wireguard.keyFormat')
  else if (peers.data?.some((each) => each.publicKey === peer.publicKey)) peerErrors.publicKey = t('network.wireguard.keyTaken')
  if (!allowed.length || allowed.some((item) => !parseCidr(item) && !item.includes(':'))) peerErrors.allowedIps = t('network.wireguard.allowedFormat')
  if (peer.endpoint && !/^(\[[0-9a-fA-F:]+\]|[A-Za-z0-9.-]+):\d{1,5}$/.test(peer.endpoint)) peerErrors.endpoint = t('network.wireguard.endpointFormat')
  if (peer.keepalive && !(Number(peer.keepalive) >= 1 && Number(peer.keepalive) <= 65535)) peerErrors.keepalive = t('network.wireguard.keepaliveRange')
  const addPeer = () => api<WireguardPeer>(`/api/v1/network/${encodeURIComponent(name)}/peers`, json('POST', {
    ...(peer.name.trim() ? { name: peer.name.trim() } : {}),
    publicKey: peer.publicKey,
    ...(peer.presharedKey ? { presharedKey: peer.presharedKey } : {}),
    allowedIps: allowed,
    ...(peer.endpoint ? { endpoint: peer.endpoint } : {}),
    ...(peer.keepalive ? { persistentKeepalive: Number(peer.keepalive) } : {}),
  })).then(() => {
    setPeer(EMPTY_PEER)
    invalidatePeers()
  })

  return (
    <>
      <div className="grid gap-3">
        <Button className="justify-self-start" size="sm" variant="outline" onClick={() => void navigate({ to: '/network', search: { tab: 'wireguard' } })}><ChevronLeft />WireGuard</Button>
        <div className="flex flex-wrap items-center gap-2.5"><Led tone={status.tone} className="size-2.5" /><h2 className="font-mono text-xl font-semibold">{name}</h2><span className="text-sm text-muted-foreground">WireGuard</span></div>
        <StatusLine status={status} />
      </div>

      <PageSection title={t('network.wireguard.tunnel')} description={t(draft ? 'network.wireguard.tunnelEditing' : 'network.wireguard.tunnelCopy')}>
        <Panel className={draft ? 'ring-2 ring-primary/40' : undefined}>
          {draft ? (
            <>
              <FormField label={t('network.fields.tunnelAddress')} hint={errors.address ? <span className="text-destructive">{errors.address}</span> : t('network.wizard.tunnelHint')}>
                {(id) => <Input id={id} className="font-mono" value={draft.address} aria-invalid={Boolean(errors.address)} onChange={(event) => setDraft({ ...draft, address: event.target.value.trim() })} />}
              </FormField>
              <FormField label={t('network.fields.listenPort')} hint={errors.port ? <span className="text-destructive">{errors.port}</span> : t('network.wireguard.portChange')}>
                {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={draft.port} onChange={(event) => setDraft({ ...draft, port: event.target.value.replace(/\D/g, '') })} />}
              </FormField>
            </>
          ) : (
            <>
              <div><Button size="sm" onClick={() => setDraft({ address: staged.static?.address ?? '', port: String(port ?? '') })}>{t('network.detail.edit')}</Button></div>
              {tunnelPending ? <Callout tone="warning" title={t('network.wireguard.tunnelPending')} /> : null}
              <FactList facts={[
                { id: 'address', label: t('network.fields.tunnelAddress'), value: staged.static?.address || '—', mono: true },
                { id: 'port', label: t('network.fields.listenPort'), value: <span><span className="font-mono">{port ?? t('network.values.automatic')}</span> <span className="text-muted-foreground">{t('network.wireguard.listenCopy')}</span></span> },
              ]} />
            </>
          )}
        </Panel>
      </PageSection>

      <PageSection title={t('network.wireguard.localKey')} description={t('network.wireguard.keyCopy')}>
        <Panel>
          {rotate.data ? <Callout tone="warning" title={t('network.wireguard.rotatedTitle')}>{t('network.wireguard.rotatedCopy')}</Callout> : null}
          {key ? <CopyField value={key} label={t('common.actions.copy')} /> : <p className="text-sm text-muted-foreground">{t('network.wireguard.keyHidden')}</p>}
          <div>
            <ConfirmDialog
              trigger={<Button size="sm" variant="destructive">{t('network.wireguard.rotate')}</Button>}
              title={t('network.wireguard.rotate')}
              description={t('network.wireguard.rotateConfirm', { name })}
              confirmLabel={t('network.wireguard.rotate')}
              success={t('network.wireguard.rotatedKey')}
              failure={t('network.wireguard.rotate')}
              onConfirm={() => rotate.mutateAsync()}
            />
          </div>
        </Panel>
      </PageSection>

      {key ? (
        <PageSection title={t('network.wireguard.localTitle')} description={t('network.wireguard.localCopy')}>
          <Panel>
            <CopyField value={snippet} label={t('common.actions.copy')} />
            <p className="text-sm text-muted-foreground">{t('network.wireguard.endpointNote')}</p>
          </Panel>
        </PageSection>
      ) : null}

      <PageSection title={t('network.wireguard.peersCount', { count: peers.data?.length ?? 0 })} description={t('network.wireguard.peersCopy')}>
        {peers.error ? <Callout tone="danger" title={failureDetail(peers.error, t('common.requestFailed'))} /> : null}
        <CollectionPanel title={t('network.wireguard.peers')} action={<Button size="sm" variant="outline" onClick={() => setAdding(true)}><Plus />{t('network.wireguard.add')}</Button>}>
          <DataTable<WireguardPeer>
            rows={peers.data}
            rowKey={(each) => each.publicKey}
            isPending={peers.isPending}
            empty={t('network.wireguard.noPeers')}
            columns={[
              { id: 'key', header: t('network.wireguard.peer'), cell: (each) => (
                <span className="grid">
                  <strong className={each.name ? undefined : 'font-normal text-muted-foreground'}>{each.name || t('network.wireguard.unnamed')}</strong>
                  <span className="text-[0.8125rem] text-muted-foreground"><span className="font-mono" title={each.publicKey}>{shortKey(each.publicKey)}</span>{each.presharedKey ? <span> · {t('network.wireguard.withPsk')}</span> : null}</span>
                </span>
              ) },
              { id: 'endpoint', header: t('network.wireguard.endpoint'), cell: (each) => each.endpoint ? <span className="font-mono text-[0.8125rem]">{each.endpoint}</span> : <span className="text-muted-foreground">{t('network.wireguard.waiting')}</span> },
              { id: 'allowed', header: t('network.wireguard.allowed'), cell: (each) => <span className="font-mono text-[0.8125rem]">{each.allowedIps.join(', ')}</span> },
              { id: 'keepalive', header: t('network.wireguard.keepalive'), cell: (each) => each.persistentKeepalive ? t('network.values.seconds', { count: each.persistentKeepalive }) : '—' },
              { id: 'actions', header: '', align: 'end', cell: (each) => (
                <ConfirmDialog
                  trigger={<Button type="button" size="sm" variant="destructive" aria-label={t('network.wireguard.remove')}><Trash2 />{t('network.wireguard.remove')}</Button>}
                  title={t('network.wireguard.remove')}
                  description={t('network.wireguard.removeCopy')}
                  confirmLabel={t('network.wireguard.remove')}
                  success={t('network.wireguard.removed')}
                  failure={t('network.wireguard.remove')}
                  onConfirm={() => remove.mutateAsync(each.publicKey)}
                />
              ) },
            ]}
          />
        </CollectionPanel>
      </PageSection>

      <FormDialog
        open={adding}
        onOpenChange={setAdding}
        title={t('network.wireguard.add')}
        description={t('network.wireguard.addCopy', { iface: name })}
        submitLabel={t('common.actions.add')}
        success={t('network.wireguard.added')}
        failure={t('network.wireguard.add')}
        onSubmit={addPeer}
        submitDisabled={Object.keys(peerErrors).length > 0}
      >
        <FormField label={t('network.wireguard.name')} hint={peerErrors.name ? <span className="text-destructive">{peerErrors.name}</span> : t('network.wireguard.nameHint')}>
          {(id) => <Input id={id} value={peer.name} onChange={(event) => setPeer({ ...peer, name: event.target.value })} />}
        </FormField>
        <FormField label={t('network.wireguard.publicKey')} hint={peer.publicKey && peerErrors.publicKey ? <span className="text-destructive">{peerErrors.publicKey}</span> : t('network.wireguard.keyHint')}>
          {(id) => <Input id={id} className="font-mono" value={peer.publicKey} onChange={(event) => setPeer({ ...peer, publicKey: event.target.value.trim() })} required />}
        </FormField>
        <FormField label={t('network.wireguard.presharedKey')} hint={peerErrors.presharedKey ? <span className="text-destructive">{peerErrors.presharedKey}</span> : t('network.wireguard.presharedHint')}>
          {(id) => <Input id={id} className="font-mono" value={peer.presharedKey} placeholder={t('network.wireguard.presharedNone')} onChange={(event) => setPeer({ ...peer, presharedKey: event.target.value.trim() })} />}
        </FormField>
        <FormField label={t('network.wireguard.endpoint')} hint={peerErrors.endpoint ? <span className="text-destructive">{peerErrors.endpoint}</span> : t('network.wireguard.endpointHint')}>
          {(id) => <Input id={id} className="font-mono" value={peer.endpoint} placeholder="vpn.example.com:51820" onChange={(event) => setPeer({ ...peer, endpoint: event.target.value.trim() })} />}
        </FormField>
        <FormField label={t('network.wireguard.allowed')} hint={peer.allowedIps && peerErrors.allowedIps ? <span className="text-destructive">{peerErrors.allowedIps}</span> : t('network.wireguard.allowedHint')}>
          {(id) => <Input id={id} className="font-mono" value={peer.allowedIps} placeholder="10.1.1.2/32, 192.168.20.0/24" onChange={(event) => setPeer({ ...peer, allowedIps: event.target.value })} required />}
        </FormField>
        <FormField label={t('network.wireguard.keepalive')} hint={peerErrors.keepalive ? <span className="text-destructive">{peerErrors.keepalive}</span> : t('network.wireguard.keepaliveHint')}>
          {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={peer.keepalive} onChange={(event) => setPeer({ ...peer, keepalive: event.target.value.replace(/\D/g, '') })} />}
        </FormField>
      </FormDialog>

      {draft ? (
        <EditBar
          what={t('network.wireguard.tunnelOf', { name })}
          dirty={draft.address !== (staged.static?.address ?? '') || draft.port !== String(port ?? '')}
          errors={Object.keys(errors).length}
          onCancel={() => setDraft(undefined)}
          onDone={saveTunnel}
          doneLabel={t('network.wireguard.stage')}
        />
      ) : null}
    </>
  )
}
