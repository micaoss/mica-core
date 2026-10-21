import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { ChevronLeft, Plus, X } from 'lucide-react'
import { cn } from '@/shared/lib/utils'
import { Callout } from '@/shared/components/callout'
import { FactList } from '@/shared/components/fact-list'
import { FormField } from '@/shared/components/form-field'
import { Page, PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { Button, buttonVariants } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { notifySuccess } from '@/shared/feedback/toast'
import { ChangeBar, EditBar } from './change-bar'
import { updateChangeSet } from './change-set'
import {
  bridgeOf, defaultPool, overlaps, poolFromRange, poolRange, stageRemove, stageRevert, stageSet, vlansOn,
  type ChangeSet, type IfaceEntry,
} from './model'
import { Led, StatusLine, Tag, useKindLabel } from './parts'
import { useNetwork, type NetworkData } from './use-network'
import { addressBook, validateEntry, type Problem, type Problems } from './validate'
import { computeStatus, exitOf } from './view'

/// One interface: what the device observes on it, and its configuration,
/// edited into the staged change set. Nothing here writes to the device.
export function InterfaceDetailPage() {
  const { name } = useParams({ from: '/network_/$name' })
  const data = useNetwork()
  const { t } = useTranslation()
  const [snapshot, setSnapshot] = useState<ChangeSet>()
  const editing = snapshot !== undefined && data.set !== undefined
  const iface = data.staged.ifaces[name]

  if (!iface) {
    return (
      <Page>
        <Back />
        <Callout tone={data.network.isPending ? 'neutral' : 'warning'} title={data.network.isPending ? t('common.states.pending') : t('network.detail.unknown', { name })} />
        <ChangeBar data={data} />
      </Page>
    )
  }

  const start = () => setSnapshot(data.set)
  const cancel = () => { if (snapshot) updateChangeSet(() => snapshot); setSnapshot(undefined) }
  const dirty = editing && JSON.stringify(snapshot?.staged) !== JSON.stringify(data.set?.staged)
  const done = () => { setSnapshot(undefined); if (dirty) notifySuccess(t('network.edit.staged')) }

  return (
    <Page>
      <Header data={data} name={name} editing={editing} />
      <Body key={name} data={data} name={name} editing={editing} onEdit={start} />
      {editing ? <EditFooter data={data} name={name} dirty={dirty} onCancel={cancel} onDone={done} /> : <ChangeBar data={data} />}
    </Page>
  )
}

function Back() {
  const { t } = useTranslation()
  return <Link to="/network" className={buttonVariants({ variant: 'outline', size: 'sm' })}><ChevronLeft aria-hidden="true" />{t('network.title')} / {t('network.tabs.interfaces')}</Link>
}

function EditFooter({ data, name, dirty, onCancel, onDone }: { data: NetworkData; name: string; dirty: boolean; onCancel: () => void; onDone: () => void }) {
  const problems = useProblems(data, name)
  return <EditBar what={name} dirty={dirty} errors={Object.keys(problems).length} onCancel={onCancel} onDone={onDone} />
}

function useProblems(data: NetworkData, name: string): Problems {
  const view = data.staged
  const iface = view.ifaces[name]
  const observed = Object.fromEntries(view.order.map((each) => [each, view.ifaces[each].obs?.address]))
  return validateEntry(name, iface.kind, view.map[name], view.map, addressBook(view.map, observed), bridgeOf(view.map, name) !== undefined)
}

function Header({ data, name, editing }: { data: NetworkData; name: string; editing: boolean }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const kindLabel = useKindLabel()
  const iface = data.staged.ifaces[name]
  const status = computeStatus(data.applied, name)
  const map = data.staged.map
  const bridge = bridgeOf(map, name)
  const vlans = vlansOn(map, name)
  const pending = data.pending.find((change) => change.name === name)
  const editable = iface.kind !== 'wifi' && iface.kind !== 'wireguard'
  return (
    <header className="grid gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <Back />
        <Select value={name} onValueChange={(value) => void navigate({ to: '/network/$name', params: { name: String(value) } })}>
          <SelectTrigger size="sm" className="w-40 font-mono" aria-label={t('network.detail.switch')}><SelectValue /></SelectTrigger>
          <SelectContent>{data.staged.order.map((each) => <SelectItem key={each} value={each}>{each}</SelectItem>)}</SelectContent>
        </Select>
      </div>
      <div className="flex flex-wrap items-center gap-2.5">
        <Led tone={status.tone} className="size-2.5" />
        <h1 className="font-mono text-xl font-semibold tracking-tight">{name}</h1>
        <span className="text-sm text-muted-foreground">{kindLabel(iface)}</span>
        {exitOf(data.applied) === name ? <Tag tone="exit">{t('network.tags.exit')}</Tag> : null}
        {data.session === name ? <Tag tone="info">{t('network.tags.session')}</Tag> : null}
        {editable && !(data.applied.ifaces[name]?.declared ?? true) ? <Tag>{t('network.tags.imageDefault')}</Tag> : null}
        {bridge ? <Tag>{t('network.detail.portOf')} <Link className="font-mono text-primary" to="/network/$name" params={{ name: bridge }}>{bridge}</Link></Tag> : null}
        {iface.kind === 'vlan' && iface.entry.vlan ? <Tag>{t('network.detail.on')} <Link className="font-mono text-primary" to="/network/$name" params={{ name: iface.entry.vlan.parent }}>{iface.entry.vlan.parent}</Link></Tag> : null}
        {vlans.map((vlan) => <Tag key={vlan}>VLAN <Link className="font-mono text-primary" to="/network/$name" params={{ name: vlan }}>{vlan}</Link></Tag>)}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <StatusLine status={status} />
        <span className="flex-1" />
        {editing ? <Tag tone="info">{t('network.detail.editing')}</Tag> : null}
        {!editing && pending ? (
          <Tag tone="pending">
            {t(pending.op === 'create' ? 'network.tags.pendingCreate' : 'network.tags.pendingChanges')}
            <button type="button" className="ms-1 text-primary hover:underline" onClick={() => updateChangeSet((set) => stageRevert(set, name))}>{t('network.review.revert')}</button>
          </Tag>
        ) : null}
      </div>
    </header>
  )
}

function Body({ data, name, editing, onEdit }: { data: NetworkData; name: string; editing: boolean; onEdit: () => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const iface = data.staged.ifaces[name]
  const applied = data.applied.ifaces[name]
  const map = data.staged.map
  const bridge = bridgeOf(map, name)
  const vlans = vlansOn(map, name)
  const editable = iface.kind !== 'wifi' && iface.kind !== 'wireguard'
  const blockers = [...(vlans.length ? [t('network.detail.blockVlans', { vlans: vlans.join(', ') })] : []), ...(bridge ? [t('network.detail.blockBridge', { bridge })] : [])]
  const physical = iface.kind === 'physical' || iface.kind === 'can'

  return (
    <>
      {data.session === name ? <Callout tone="neutral" title={t('network.detail.sessionTitle', { name })}>{t('network.detail.sessionCopy')}</Callout> : null}
      {applied ? <Observation data={data} name={name} /> : null}
      {!editable ? (
        <PageSection title={t('network.detail.config')}>
          <Panel>
            <Button className="justify-self-start" variant="outline" onClick={() => void navigate({ to: '/network', search: iface.kind === 'wifi' ? { tab: 'wifi' } : { tab: 'wireguard', tunnel: name } })}>
              {t(iface.kind === 'wifi' ? 'network.detail.onWifi' : 'network.detail.onWireguard')}
            </Button>
          </Panel>
        </PageSection>
      ) : (
        <PageSection title={t('network.detail.config')} description={editing ? t('network.detail.editHint') : undefined}>
          <Panel className={editing ? 'ring-2 ring-primary/40' : undefined}>
            {editing ? <ConfigForm data={data} name={name} /> : <ConfigView data={data} name={name} onEdit={onEdit} />}
          </Panel>
        </PageSection>
      )}
      {editable && !editing && (!physical || iface.declared) ? (
        <PageSection title={t(physical ? 'network.detail.restore' : 'network.detail.delete')} tone="danger">
          <Panel>
            <p className="text-sm text-muted-foreground">
              {physical ? t('network.detail.restoreCopy') : iface.kind === 'bridge' && iface.entry.bridge?.ports.length ? t('network.detail.deleteBridgeCopy', { ports: iface.entry.bridge.ports.join(', ') }) : t('network.detail.deleteCopy')}
            </p>
            {blockers.length ? <p className="text-sm text-destructive">{t('network.detail.blocked', { reasons: blockers.join('; ') })}</p> : null}
            <div className="flex flex-wrap items-center gap-3">
              <Button
                size="sm"
                variant="destructive"
                disabled={blockers.length > 0}
                onClick={() => {
                  updateChangeSet((set) => stageRemove(set, name, iface.kind))
                  if (!physical) void navigate({ to: '/network' })
                }}
              >
                {physical ? t('network.detail.restore') : t('network.detail.deleteNamed', { name })}
              </Button>
              <span className="text-sm text-muted-foreground">{t('network.detail.stagedHint')}</span>
            </div>
          </Panel>
        </PageSection>
      ) : null}
    </>
  )
}

function Observation({ data, name }: { data: NetworkData; name: string }) {
  const { t } = useTranslation()
  const iface = data.applied.ifaces[name]
  const obs = iface.obs
  const entry = iface.entry
  const map = data.applied.map
  const link = !obs
    ? t('network.list.missing')
    : iface.kind === 'bridge'
      ? t('network.status.notes.portsUp', { up: (entry.bridge?.ports ?? []).filter((port) => data.applied.ifaces[port]?.obs?.carrier).length, count: entry.bridge?.ports.length ?? 0 })
      : iface.kind === 'vlan'
        ? t(data.applied.ifaces[entry.vlan?.parent ?? '']?.obs?.carrier ? 'network.detail.parentUp' : 'network.detail.parentDown')
        : t(obs.carrier ? 'network.list.linkUp' : 'network.list.noCable')
  const gateway = obs?.lease?.router ?? (!entry.dhcp && obs?.address ? entry.static?.gateway : undefined)
  const facts = iface.kind === 'can'
    ? [
        { id: 'state', label: t('network.fields.state'), value: obs?.operationalState ?? '—' },
        { id: 'bitrate', label: t('network.fields.bitrate'), value: t('network.status.notes.bitrateUnreported') },
      ]
    : [
        { id: 'link', label: t('network.fields.link'), value: link },
        { id: 'address', label: t('network.fields.address'), value: obs?.address ? <span><span className="font-mono">{obs.address}</span> <span className="text-muted-foreground">{obs.source}</span></span> : '—' },
        { id: 'gateway', label: t('network.fields.gateway'), value: gateway ?? '—', mono: true },
        { id: 'dns', label: t('network.fields.dns'), value: obs?.dns.length ? obs.dns.join(', ') : '—', mono: true },
        obs?.lease?.lifetimeSeconds !== undefined && { id: 'lease', label: t('network.fields.leaseLeft'), value: t('network.values.seconds', { count: obs.lease.lifetimeSeconds }) },
        { id: 'mac', label: t('network.fields.mac'), value: obs?.mac ?? '—', mono: true },
        bridgeOf(map, name) && { id: 'bridge', label: t('network.list.memberOf'), value: bridgeOf(map, name), mono: true },
      ]
  return (
    <PageSection title={t('network.detail.observed')}>
      <Panel><FactList facts={facts} /></Panel>
    </PageSection>
  )
}

const mono = (value: ReactNode) => <span className="font-mono">{value}</span>

function ConfigView({ data, name, onEdit }: { data: NetworkData; name: string; onEdit: () => void }) {
  const { t } = useTranslation()
  const iface = data.staged.ifaces[name]
  const entry = iface.entry
  const map = data.staged.map
  const bridge = bridgeOf(map, name)
  const server = !entry.dhcp && entry.static && entry.dhcpServer ? entry.dhcpServer : undefined
  const range = server ? poolRange(entry.static?.address, server) : undefined
  return (
    <div className="grid gap-5">
      <Button className="justify-self-start" size="sm" onClick={onEdit}>{t('network.detail.edit')}</Button>
      {iface.kind === 'can' ? (
        <Group title={t('network.can.title')}>
          <FactList facts={[
            { id: 'bitrate', label: t('network.fields.bitrate'), value: entry.can ? `${entry.can.bitrate}` : t('network.values.notSet'), mono: true },
            { id: 'restart', label: t('network.fields.restart'), value: entry.can?.restartMs ? `${entry.can.restartMs} ms` : t('network.values.never') },
            { id: 'fd', label: t('network.fields.fd'), value: t(entry.can?.fd ? 'network.values.on' : 'network.values.off') },
            entry.can?.fd && { id: 'data', label: t('network.fields.dataBitrate'), value: entry.can.dataBitrate ?? '—', mono: true },
          ]} />
        </Group>
      ) : bridge ? (
        <Group title={t('network.detail.addressing')}>
          <p className="text-sm text-muted-foreground">{t('network.detail.portNote', { bridge })}</p>
        </Group>
      ) : (
        <>
          <Group title={t('network.detail.preset')}>
            <strong className="text-sm">{t(entry.dhcp ? 'network.presets.dhcp' : entry.static ? 'network.presets.static' : 'network.values.noAddress')}</strong>
          </Group>
          <Group title={t(entry.dhcp ? 'network.fields.dns' : 'network.detail.addressAndDns')}>
            <FactList facts={entry.dhcp
              ? [{ id: 'dns', label: t('network.fields.dns'), value: entry.dns?.length ? mono(entry.dns.join(', ')) : t('network.detail.leaseDns') }]
              : [
                  { id: 'address', label: t('network.fields.address'), value: mono(entry.static?.address || t('network.values.notSet')) },
                  { id: 'gateway', label: t('network.fields.gateway'), value: mono(entry.static?.gateway || t('network.values.none')) },
                  { id: 'dns', label: t('network.fields.dns'), value: mono(entry.static?.dns?.length ? entry.static.dns.join(', ') : t('network.values.none')) },
                ]} />
          </Group>
          {!entry.dhcp && entry.static && !data.openrc ? (
            <Group title={t('network.fields.dhcpServer')}>
              <FactList facts={server ? [
                { id: 'state', label: t('network.fields.state'), value: t('network.values.on') },
                { id: 'range', label: t('network.fields.poolRange'), value: mono(range ? `${range.start} – ${range.end}` : '—') },
                { id: 'lease', label: t('network.fields.lease'), value: server.leaseSeconds ? t('network.values.seconds', { count: server.leaseSeconds }) : t('network.values.automatic') },
                { id: 'dns', label: t('network.fields.serverDns'), value: server.dns?.length ? mono(server.dns.join(', ')) : t('network.values.uplinkDns') },
              ] : [{ id: 'state', label: t('network.fields.state'), value: t('network.values.off') }]} />
            </Group>
          ) : null}
        </>
      )}
      {iface.kind === 'bridge' ? (
        <Group title={t('network.detail.portsCount', { count: entry.bridge?.ports.length ?? 0 })}>
          {entry.bridge?.ports.length ? (
            <div className="flex flex-wrap gap-3">
              {entry.bridge.ports.map((port) => {
                const status = computeStatus(data.applied, port)
                return <span key={port} className="flex items-center gap-1.5 text-sm"><Led tone={status.tone} /><Link className="font-mono font-medium text-primary" to="/network/$name" params={{ name: port }}>{port}</Link><span className="text-muted-foreground">{t(`network.status.${status.label}`)}</span></span>
              })}
            </div>
          ) : <p className="text-sm text-muted-foreground">{t('network.detail.noPorts')}</p>}
        </Group>
      ) : null}
      {iface.kind === 'vlan' && entry.vlan ? (
        <Group title="VLAN">
          <FactList facts={[
            { id: 'parent', label: t('network.fields.parent'), value: <Link className="font-mono text-primary" to="/network/$name" params={{ name: entry.vlan.parent }}>{entry.vlan.parent}</Link> },
            { id: 'id', label: t('network.fields.vlanId'), value: entry.vlan.id, mono: true },
          ]} />
        </Group>
      ) : null}
      {!bridge && iface.kind !== 'can' && !data.openrc ? (
        <Group title={t('network.fields.routes')}>
          {entry.routes?.length ? <div className="grid gap-1 font-mono text-sm">{entry.routes.map((route) => <span key={`${route.destination}-${route.gateway ?? ''}`}>{route.destination} → {route.gateway || t('network.values.onLink')}</span>)}</div> : <p className="text-sm text-muted-foreground">{t('network.values.none')}</p>}
        </Group>
      ) : null}
    </div>
  )
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="grid gap-2">
      <h3 className="text-sm font-semibold">{title}</h3>
      {children}
    </section>
  )
}

const splitList = (text: string) => text.split(/[,，\s]+/).map((item) => item.trim()).filter(Boolean)

/// Several addresses typed into one box. The box keeps what was typed; the
/// entry gets the parsed list, invalid items included, so validation names them.
function ListInput({ id, values, onChange, placeholder, invalid }: { id: string; values: string[] | undefined; onChange: (values: string[]) => void; placeholder?: string; invalid?: boolean }) {
  const [text, setText] = useState((values ?? []).join(', '))
  const shown = splitList(text).join(',') === (values ?? []).join(',') ? text : (values ?? []).join(', ')
  return <Input id={id} className="font-mono" value={shown} placeholder={placeholder} aria-invalid={invalid} onChange={(event) => { setText(event.target.value); onChange(splitList(event.target.value)) }} />
}

function ProblemText({ problem }: { problem?: Problem }) {
  const { t } = useTranslation()
  return problem ? <span className="text-destructive">{t(problem.key, problem.params)}</span> : null
}

function ConfigForm({ data, name }: { data: NetworkData; name: string }) {
  const { t } = useTranslation()
  const iface = data.staged.ifaces[name]
  const applied = data.applied.ifaces[name]
  const entry = iface.entry
  const map = data.staged.map
  const bridge = bridgeOf(map, name)
  const problems = useProblems(data, name)
  const set = (change: (entry: IfaceEntry) => IfaceEntry) => updateChangeSet((current) => stageSet(current, name, iface.kind, change))
  const [customDns, setCustomDns] = useState(Boolean(entry.dns?.length))

  if (iface.kind === 'can') return <CanForm data={data} name={name} problems={problems} />
  if (bridge) {
    return (
      <Group title={t('network.detail.addressing')}>
        <p className="text-sm text-muted-foreground">{t('network.detail.portNote', { bridge })}</p>
        <Button className="justify-self-start" size="sm" variant="outline" onClick={() => updateChangeSet((current) => stageSet(current, bridge, 'bridge', (each) => ({ ...each, bridge: { ports: (each.bridge?.ports ?? []).filter((port) => port !== name) } })))}>
          {t('network.detail.leaveBridge', { bridge })}
        </Button>
      </Group>
    )
  }

  const toDhcp = () => set((current) => {
    const next: IfaceEntry = { ...current, dhcp: true }
    delete next.static
    delete next.dhcpServer
    delete next.dns
    return next
  })
  const toStatic = () => set((current) => {
    const next: IfaceEntry = { ...current, dhcp: false, static: { address: current.static?.address || suggestSubnet(data), ...(current.static?.gateway ? { gateway: current.static.gateway } : {}), dns: current.dns ?? current.static?.dns ?? [] } }
    delete next.dns
    return next
  })
  const lease = applied?.obs
  const canServe = !entry.dhcp && entry.static !== undefined && !data.openrc
  const pool = entry.static?.address ? defaultPool(entry.static.address) : undefined
  const candidates = data.staged.order.filter((each) => data.staged.ifaces[each].kind === 'physical' && each !== name)

  return (
    <div className="grid gap-6">
      <Group title={t('network.detail.preset')}>
        <SegmentedControl<'dhcp' | 'static'>
          label={t('network.detail.preset')}
          value={entry.dhcp ? 'dhcp' : 'static'}
          onValueChange={(preset) => (preset === 'dhcp' ? toDhcp : toStatic)()}
          segments={(['dhcp', 'static'] as const).map((preset) => ({ value: preset, label: t(`network.presets.${preset}`) }))}
        />
        <p className="text-sm text-muted-foreground">{t(`network.presets.${entry.dhcp ? 'dhcp' : 'static'}Copy`)}</p>
      </Group>

      {entry.dhcp ? (
        <Group title={t('network.fields.dns')}>
          <label className="flex items-center justify-between gap-3 text-sm">
            <span className="grid gap-0.5"><span className="font-medium">{t('network.fields.customDns')}</span><span className="text-muted-foreground">{t('network.detail.customDnsHint')}</span></span>
            <Switch checked={customDns} onCheckedChange={(on) => { setCustomDns(on); if (!on) set((current) => ({ ...current, dns: [] })) }} aria-label={t('network.fields.customDns')} />
          </label>
          {customDns ? (
            <FormField label={t('network.fields.dns')} hint={problems.dns ? <ProblemText problem={problems.dns} /> : t('network.detail.listHint')}>
              {(id) => <ListInput id={id} values={entry.dns} placeholder="9.9.9.9, 1.1.1.1" invalid={Boolean(problems.dns)} onChange={(dns) => set((current) => ({ ...current, dns }))} />}
            </FormField>
          ) : null}
        </Group>
      ) : (
        <Group title={t('network.detail.addressAndDns')}>
          <FormField label={t('network.fields.address')} hint={problems.address ? <ProblemText problem={problems.address} /> : t('network.detail.cidrHint')}>
            {(id) => <Input id={id} className="font-mono" value={entry.static?.address ?? ''} placeholder="192.168.10.1/24" aria-invalid={Boolean(problems.address)} onChange={(event) => set((current) => ({ ...current, static: { ...current.static, address: event.target.value.trim() } }))} />}
          </FormField>
          {lease?.address && /dhcp/i.test(lease.source ?? '') && !entry.static?.address ? (
            <Button className="justify-self-start" size="sm" variant="link" onClick={() => set((current) => ({ ...current, static: { address: lease.address ?? '', ...(lease.lease?.router ? { gateway: lease.lease.router } : {}), dns: lease.dns } }))}>
              {t('network.detail.useLease', { address: lease.address })}
            </Button>
          ) : null}
          <FormField label={t('network.fields.gateway')} hint={problems.gateway ? <ProblemText problem={problems.gateway} /> : t('network.detail.gatewayHint')}>
            {(id) => <Input id={id} className="font-mono" value={entry.static?.gateway ?? ''} placeholder={t('network.values.none')} aria-invalid={Boolean(problems.gateway)} onChange={(event) => set((current) => { const value = event.target.value.trim(); const next = { ...current, static: { address: current.static?.address ?? '', ...current.static } }; if (value) next.static.gateway = value; else delete next.static.gateway; return next })} />}
          </FormField>
          <FormField label={t('network.fields.dns')} hint={problems.dns ? <ProblemText problem={problems.dns} /> : t('network.detail.listHint')}>
            {(id) => <ListInput id={id} values={entry.static?.dns} placeholder={t('network.detail.optional')} invalid={Boolean(problems.dns)} onChange={(dns) => set((current) => ({ ...current, static: { address: current.static?.address ?? '', ...current.static, dns } }))} />}
          </FormField>
        </Group>
      )}

      {canServe ? <DhcpServerForm data={data} name={name} problems={problems} pool={pool} /> : null}

      {iface.kind === 'bridge' ? (
        <Group title={t('network.detail.portsCount', { count: entry.bridge?.ports.length ?? 0 })}>
          <div role="group" aria-label={t('network.fields.ports')} className="grid gap-2 sm:grid-cols-2">
            {candidates.map((port) => {
              const other = bridgeOf(map, port)
              const elsewhere = other !== undefined && other !== name
              const on = entry.bridge?.ports.includes(port) ?? false
              const status = computeStatus(data.applied, port)
              return (
                <label key={port} className={cn('flex items-center gap-2 rounded-lg border p-2 text-sm', on && 'border-primary', elsewhere && 'opacity-55')}>
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={on}
                    disabled={elsewhere}
                    onChange={(event) => set((current) => ({ ...current, bridge: { ports: event.target.checked ? [...(current.bridge?.ports ?? []), port] : (current.bridge?.ports ?? []).filter((each) => each !== port) } }))}
                  />
                  <span className="font-mono font-medium">{port}</span>
                  <Led tone={status.tone} />
                  <span className="text-muted-foreground">{elsewhere ? t('network.detail.inOther', { bridge: other }) : t(`network.status.${status.label}`)}</span>
                </label>
              )
            })}
          </div>
          <p className="text-sm text-muted-foreground">{t('network.detail.portsHint')}</p>
          {(entry.bridge?.ports ?? []).map((port) => port === data.session
            ? <p key={port} className="text-sm text-destructive">{t('network.detail.portIsSession', { port })}</p>
            : !(data.set?.base[name]?.bridge?.ports ?? []).includes(port) && data.staged.ifaces[port]?.obs?.address
              ? <p key={port} className="text-sm text-muted-foreground">{t('network.detail.portLosesAddress', { port })}</p>
              : null)}
        </Group>
      ) : null}

      {iface.kind === 'vlan' && entry.vlan ? (
        <Group title="VLAN">
          <FormField label={t('network.fields.parent')} hint={t('network.detail.vlanRebuild')}>
            {(id) => (
              <Select value={entry.vlan?.parent ?? ''} onValueChange={(value) => set((current) => ({ ...current, vlan: { id: current.vlan?.id ?? 0, parent: String(value) } }))}>
                <SelectTrigger id={id} className="w-48 font-mono"><SelectValue /></SelectTrigger>
                <SelectContent>{data.staged.order.filter((each) => ['physical', 'bridge'].includes(data.staged.ifaces[each].kind) && !bridgeOf(map, each)).map((each) => <SelectItem key={each} value={each}>{each}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
          <FormField label={t('network.fields.vlanId')} hint={problems.vlanId ? <ProblemText problem={problems.vlanId} /> : undefined}>
            {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={entry.vlan?.id ?? ''} aria-invalid={Boolean(problems.vlanId)} onChange={(event) => set((current) => ({ ...current, vlan: { parent: current.vlan?.parent ?? '', id: Number(event.target.value.replace(/\D/g, '')) || 0 } }))} />}
          </FormField>
        </Group>
      ) : null}

      {!data.openrc ? <RoutesForm data={data} name={name} problems={problems} /> : null}
    </div>
  )
}

/// A subnet for an interface switched to static: the first 192.168.<n>.1/24
/// that no interface uses.
function suggestSubnet(data: NetworkData): string {
  const used = data.staged.order
    .map((name) => data.staged.ifaces[name].obs?.address ?? (!data.staged.map[name]?.dhcp ? data.staged.map[name]?.static?.address : undefined))
    .filter((address): address is string => Boolean(address))
  for (let third = 10; third < 250; third += 10) {
    const candidate = `192.168.${third}.1/24`
    if (!used.some((address) => overlaps(candidate, address))) return candidate
  }
  return '192.168.200.1/24'
}

function DhcpServerForm({ data, name, problems, pool }: { data: NetworkData; name: string; problems: Problems; pool?: { poolOffset: number; poolSize: number } }) {
  const { t } = useTranslation()
  const iface = data.staged.ifaces[name]
  const entry = iface.entry
  const address = entry.static?.address
  const server = entry.dhcpServer
  const range = server ? poolRange(address, server) : undefined
  const [start, setStart] = useState(range?.start ?? '')
  const [end, setEnd] = useState(range?.end ?? '')
  const set = (change: (entry: IfaceEntry) => IfaceEntry) => updateChangeSet((current) => stageSet(current, name, iface.kind, change))
  const typed = poolFromRange(address, start, end)
  const local = server && (start || end) && !typed ? t('network.problems.poolSubnet') : undefined
  const typeRange = (nextStart: string, nextEnd: string) => {
    setStart(nextStart); setEnd(nextEnd)
    const parsed = poolFromRange(address, nextStart, nextEnd)
    if (parsed) set((current) => ({ ...current, dhcpServer: { ...(current.dhcpServer ?? { poolOffset: 0, poolSize: 0 }), ...parsed } }))
  }
  const toggle = (on: boolean) => {
    if (!on) { set((current) => { const next = { ...current }; delete next.dhcpServer; return next }); return }
    if (!pool) return
    const range = poolRange(address, pool)
    setStart(range?.start ?? ''); setEnd(range?.end ?? '')
    set((current) => ({ ...current, dhcpServer: { ...pool } }))
  }
  return (
    <Group title={t('network.fields.dhcpServer')}>
      <label className="flex items-center justify-between gap-3 text-sm">
        <span className="grid gap-0.5"><span className="font-medium">{t('network.detail.serverOn')}</span><span className="text-muted-foreground">{t(pool ? 'network.detail.serverHint' : 'network.detail.serverNeedsAddress')}</span></span>
        <Switch checked={server !== undefined} disabled={!pool && server === undefined} onCheckedChange={toggle} aria-label={t('network.detail.serverOn')} />
      </label>
      {server ? (
        <>
          <FormField label={t('network.fields.poolRange')} hint={local || problems.pool ? <span className="text-destructive">{local ?? t(problems.pool?.key ?? '', problems.pool?.params)}</span> : undefined}>
            {(id) => (
              <div className="flex items-center gap-2">
                <Input id={id} className="font-mono" value={start} aria-label={t('network.detail.poolStart')} onChange={(event) => typeRange(event.target.value.trim(), end)} />
                <span>–</span>
                <Input className="font-mono" value={end} aria-label={t('network.detail.poolEnd')} onChange={(event) => typeRange(start, event.target.value.trim())} />
              </div>
            )}
          </FormField>
          <FormField label={t('network.fields.leaseSeconds')} hint={problems.lease ? <ProblemText problem={problems.lease} /> : undefined}>
            {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={server.leaseSeconds ?? ''} placeholder={t('network.values.automatic')} onChange={(event) => set((current) => { const digits = event.target.value.replace(/\D/g, ''); const next = { ...current, dhcpServer: { ...(current.dhcpServer ?? server) } }; if (digits) next.dhcpServer.leaseSeconds = Number(digits); else delete next.dhcpServer.leaseSeconds; return next })} />}
          </FormField>
          <FormField label={t('network.fields.serverDns')} hint={problems.serverDns ? <ProblemText problem={problems.serverDns} /> : t('network.detail.serverDnsHint')}>
            {(id) => <ListInput id={id} values={server.dns} placeholder={t('network.detail.optional')} invalid={Boolean(problems.serverDns)} onChange={(dns) => set((current) => ({ ...current, dhcpServer: { ...(current.dhcpServer ?? server), dns } }))} />}
          </FormField>
        </>
      ) : null}
    </Group>
  )
}

function RoutesForm({ data, name, problems }: { data: NetworkData; name: string; problems: Problems }) {
  const { t } = useTranslation()
  const iface = data.staged.ifaces[name]
  const routes = iface.entry.routes ?? []
  const set = (next: NonNullable<IfaceEntry['routes']>) => updateChangeSet((current) => stageSet(current, name, iface.kind, (entry) => ({ ...entry, routes: next })))
  return (
    <Group title={t('network.fields.routes')}>
      {routes.map((route, index) => (
        <div key={index} className="grid grid-cols-[1fr_1fr_auto] items-center gap-2">
          <Input className="font-mono" aria-label={t('network.detail.routeDestination')} value={route.destination} placeholder="10.20.0.0/16" onChange={(event) => set(routes.map((row, at) => at === index ? { ...row, destination: event.target.value.trim() } : row))} />
          <Input className="font-mono" aria-label={t('network.detail.routeVia')} value={route.gateway ?? ''} placeholder={t('network.detail.routeOnLink')} onChange={(event) => set(routes.map((row, at) => { if (at !== index) return row; const next = { ...row }; const value = event.target.value.trim(); if (value) next.gateway = value; else delete next.gateway; return next }))} />
          <Button size="icon-sm" variant="ghost" aria-label={t('common.actions.delete')} onClick={() => set(routes.filter((_, at) => at !== index))}><X /></Button>
        </div>
      ))}
      {problems.routes ? <ProblemText problem={problems.routes} /> : routes.length ? <p className="text-sm text-muted-foreground">{t('network.detail.routesHint')}</p> : null}
      <Button className="justify-self-start" size="sm" variant="outline" onClick={() => set([...routes, { destination: '' }])}><Plus />{t('network.detail.addRoute')}</Button>
    </Group>
  )
}

function CanForm({ data, name, problems }: { data: NetworkData; name: string; problems: Problems }) {
  const { t } = useTranslation()
  const iface = data.staged.ifaces[name]
  const can = iface.entry.can ?? { bitrate: 0 }
  const set = (change: (can: NonNullable<IfaceEntry['can']>) => NonNullable<IfaceEntry['can']>) => updateChangeSet((current) => stageSet(current, name, 'can', (entry) => ({ ...entry, kind: 'can', can: change({ ...(entry.can ?? { bitrate: 0 }) }) })))
  const digits = (text: string) => text.replace(/\D/g, '')
  return (
    <Group title={t('network.can.title')}>
      <p className="text-sm text-muted-foreground">{t('network.can.copy')}</p>
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField label={t('network.fields.bitrate')} hint={problems.bitrate ? <ProblemText problem={problems.bitrate} /> : t('network.can.bitrateHint')}>
          {(id) => <Input id={id} className="font-mono" inputMode="numeric" value={can.bitrate || ''} aria-invalid={Boolean(problems.bitrate)} onChange={(event) => set((current) => ({ ...current, bitrate: Number(digits(event.target.value)) }))} />}
        </FormField>
        <FormField label={t('network.fields.restart')} hint={t('network.can.restartHint')}>
          {(id) => <Input id={id} className="font-mono" inputMode="numeric" value={can.restartMs ?? ''} onChange={(event) => set((current) => { const next = { ...current }; if (digits(event.target.value)) next.restartMs = Number(digits(event.target.value)); else delete next.restartMs; return next })} />}
        </FormField>
      </div>
      <FormField label={t('network.fields.fd')}>
        {() => <SegmentedControl<'on' | 'off'> label={t('network.fields.fd')} value={can.fd ? 'on' : 'off'} onValueChange={(value) => set((current) => { const next = { ...current, fd: value === 'on' }; if (value === 'on') next.dataBitrate = current.dataBitrate ?? 2_000_000; else delete next.dataBitrate; return next })} segments={[{ value: 'on', label: t('network.values.on') }, { value: 'off', label: t('network.values.off') }]} />}
      </FormField>
      {can.fd ? (
        <FormField label={t('network.fields.dataBitrate')} hint={t('network.can.dataBitrateHint')}>
          {(id) => <Input id={id} className="font-mono" inputMode="numeric" value={can.dataBitrate ?? ''} onChange={(event) => set((current) => ({ ...current, dataBitrate: Number(digits(event.target.value)) || undefined }))} />}
        </FormField>
      ) : null}
      {data.session === name ? <p className="text-sm text-destructive">{t('network.detail.sessionCopy')}</p> : null}
    </Group>
  )
}
