import { useMemo, useState } from 'react'
import { Link } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Cable, ChevronRight, KeyRound, Plus, Radar, Trash2 } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import { configuredSummary, networkRows, physicalInterfaces, visibleNetworkRows, type NetworkRow } from '@/lib/network'
import type { NetworkOverview, ObservedNetworkState, TaskAccepted, WifiScan } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { CopyField } from '@/shared/components/copy-field'
import { CollectionPanel, Panel } from '@/shared/components/panel'
import { DataTable } from '@/shared/components/data-table'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField, ToggleField } from '@/shared/components/form-field'
import { MetricCard } from '@/shared/components/metric-card'
import { Page, PageHeader } from '@/shared/components/page'
import { StatusBadge } from '@/shared/components/status-badge'
import { TaskProgress } from '@/shared/components/task-progress'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { Switch } from '@/shared/components/ui/switch'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/shared/components/ui/tabs'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { failureDetail } from '@/shared/feedback/toast'
import { ObservedNetworkPanel } from '@/features/network/observed-network-panel'
import { WifiApPanel } from '@/features/network/wifi-ap-panel'
import { BluetoothPanel } from '@/features/network/bluetooth-panel'
import { formatAge, formatKnownState } from '@/i18n/format'

interface WifiNetwork { ssid: string; psk?: string; hidden: boolean; priority: number }
interface WifiClientRole { enabled: boolean; interface: string }
interface WireguardPeer { publicKey: string; allowedIps: string[]; endpoint?: string; persistentKeepalive?: number }
interface WireguardRotation { publicKey: string }
interface InterfaceConfig {
  kind?: 'physical' | 'vlan' | 'bridge' | 'wireguard'
  dhcp: boolean
  static?: { address: string; gateway?: string; dns: string[] }
  vlan?: { parent: string; id: number }
  bridge?: { ports: string[] }
  wireguard?: { listenPort?: number; peers: WireguardPeer[] }
}

export function NetworkPage() {
  const { t } = useTranslation()
  const [adding, setAdding] = useState(false)
  const network = useQuery({ queryKey: ['network'], queryFn: () => api<NetworkOverview>('/api/v1/network'), refetchInterval: 10_000 })
  const allRows = networkRows(network.data?.configured, network.data?.observed.interfaces)
  const rows = visibleNetworkRows(allRows)
  const ports = physicalInterfaces(allRows)
  const age = formatAge(Date.now() - network.dataUpdatedAt, t)
  const summaryLabels = {
    notConfigured: t('network.summary.notConfigured'),
    physical: t('network.summary.physical'),
    dhcp: t('network.summary.dhcp'),
    static: t('network.summary.static'),
    noAddressing: t('network.summary.noAddressing'),
    format: (kind: string, method: string) => t('network.summary.value', { kind, method }),
  }

  return (
    <Page>
      <PageHeader title={t('network.title')} action={<Button onClick={() => setAdding(true)}><Plus />{t('network.actions.addInterface')}</Button>} />
      {network.error ? <Callout tone="danger" title={failureDetail(network.error, t('common.requestFailed'))} /> : null}
      {network.data?.observed.error ? <Callout tone="warning" title={network.data.observed.error} /> : null}
      <Tabs defaultValue="interfaces">
        <TabsList aria-label={t('network.title')}>
          <TabsTrigger value="interfaces">{t('network.tabs.interfaces')}</TabsTrigger>
          <TabsTrigger value="wifi">{t('network.tabs.wifi')}</TabsTrigger>
          <TabsTrigger value="wireguard">{t('network.tabs.wireguard')}</TabsTrigger>
          <TabsTrigger value="bluetooth">{t('network.tabs.bluetooth')}</TabsTrigger>
          <TabsTrigger value="observed">{t('network.tabs.observed')}</TabsTrigger>
        </TabsList>
        <TabsContent value="interfaces" className="grid gap-6 pt-4">
          <CollectionPanel>
            <DataTable<NetworkRow>
              rows={rows}
              rowKey={(row) => row.name}
              isPending={network.isPending}
              empty={t('network.noInterfaces')}
              emptyIcon={<Cable />}
              columns={[
                { id: 'interface', header: t('network.table.interface'), cell: (row) => row.name },
                { id: 'type', header: t('network.table.type'), cell: (row) => kindOf(row) ? t(`network.kinds.${kindOf(row)}`, { defaultValue: kindOf(row) }) : t('common.notAvailable') },
                { id: 'configured', header: t('network.table.configured'), cell: (row) => configuredSummary(row.configured, summaryLabels) },
                { id: 'observed', header: t('network.table.observed'), cell: (row) => (
                  <StatusBadge tone={isOnline(row) ? 'success' : 'danger'}>
                    {row.observed?.operationalState ? formatKnownState(row.observed.operationalState, t) : t('network.notObserved')}
                  </StatusBadge>
                ) },
                { id: 'addresses', header: t('network.table.addresses'), cell: (row) => <span className="font-mono text-[0.8125rem] break-all">{summarize(row.observed?.addresses)}</span> },
                { id: 'age', header: t('network.table.lastObserved'), align: 'end', cell: (row) => row.observed ? age : '—' },
              ]}
              // One focus stop and one navigation per row. The row used to
              // carry an onClick beside a nested link, so a click on the name
              // navigated twice and the keyboard reached neither.
              rowHref={(row) => (
                <Link to="/network/$name" params={{ name: row.name }} className="flex items-center gap-1 font-mono font-medium text-primary hover:underline">
                  {row.name}<ChevronRight className="size-3.5" aria-hidden="true" />
                </Link>
              )}
            />
          </CollectionPanel>
        </TabsContent>
        <TabsContent value="wifi" className="grid gap-6 pt-4"><WifiPanel /></TabsContent>
        <TabsContent value="wireguard" className="grid gap-6 pt-4"><WireguardPanel configured={(network.data?.configured ?? {}) as Record<string, InterfaceConfig>} /></TabsContent>
        <TabsContent value="bluetooth" className="grid gap-6 pt-4"><BluetoothPanel /></TabsContent>
        {/* What the device sees, device-wide: routes, DNS, radios. The
            per-interface half of it is on each interface's own page, which is
            where an operator who clicked a row is already looking. */}
        <TabsContent value="observed" className="grid gap-6 pt-4"><ObservedNetworkPanel /></TabsContent>
      </Tabs>
      <InterfaceDialog key={adding ? 'new' : 'closed'} open={adding} onClose={() => setAdding(false)} ports={ports} />
    </Page>
  )
}

function kindOf(row: NetworkRow) {
  return (row.configured?.kind as string | undefined) ?? row.observed?.kind ?? row.observed?.type
}

function isOnline(row: NetworkRow) {
  return ['routable', 'carrier', 'degraded'].includes(row.observed?.operationalState ?? '')
}

function InterfaceDialog({ open, onClose, ports: candidates }: { open: boolean; onClose: () => void; ports: string[] }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [name, setName] = useState('')
  const [kind, setKind] = useState<NonNullable<InterfaceConfig['kind']>>('physical')
  const [dhcp, setDhcp] = useState(true)
  const [address, setAddress] = useState('')
  const [gateway, setGateway] = useState('')
  const [dns, setDns] = useState('')
  const [parent, setParent] = useState('')
  const [vlanId, setVlanId] = useState('100')
  const [ports, setPorts] = useState<string[]>([])
  const [listenPort, setListenPort] = useState('51820')
  // The first link the device actually has, rather than a name typed into the
  // code: an appliance whose NIC is `enp1s0` had `eth0` pre-filled here, and a
  // VLAN on an undeclared parent is refused by micad.
  const parentValue = parent || candidates[0] || ''

  // A tunnel has no DHCP client and no gateway of its own: its addressing is
  // the address on the interface, and where its traffic goes is each peer's
  // allowed IPs. Offering the switch would offer a configuration micad refuses.
  const addressing = kind === 'wireguard' ? 'static' : dhcp ? 'dhcp' : 'static'

  const save = () => {
    const value: InterfaceConfig = { dhcp: addressing === 'dhcp' }
    if (kind !== 'physical') value.kind = kind
    if (addressing === 'static') {
      value.static = {
        address,
        ...(gateway && kind !== 'wireguard' ? { gateway } : {}),
        dns: kind === 'wireguard' ? [] : splitList(dns),
      }
    }
    if (kind === 'vlan') value.vlan = { parent: parentValue, id: Number(vlanId) }
    if (kind === 'bridge') value.bridge = { ports }
    if (kind === 'wireguard') value.wireguard = { listenPort: Number(listenPort), peers: [] }
    return api<TaskAccepted>(`/api/v1/network/${encodeURIComponent(name)}`, json('PUT', value))
      .then(() => queryClient.invalidateQueries({ queryKey: ['network'] }))
  }

  return (
    <FormDialog
      open={open}
      onOpenChange={(next) => { if (!next) onClose() }}
      title={t('network.editor.addTitle')}
      description={t('network.editor.description')}
      submitLabel={t('common.actions.add')}
      success={t('network.editor.added', { name })}
      failure={t('network.editor.addTitle')}
      onSubmit={save}
    >
      <FormField label={t('network.editor.name')}>
        {(id) => <Input id={id} value={name} onChange={(event) => setName(event.target.value)} required />}
      </FormField>
      <FormField label={t('network.editor.kind')}>
        {(id) => (
          <Select value={kind} onValueChange={(value) => setKind(value as typeof kind)}>
            <SelectTrigger id={id}><SelectValue /></SelectTrigger>
            <SelectContent>{(['physical', 'vlan', 'bridge', 'wireguard'] as const).map((option) => <SelectItem value={option} key={option}>{t(`network.kinds.${option}`)}</SelectItem>)}</SelectContent>
          </Select>
        )}
      </FormField>
      {kind === 'wireguard' ? (
        <p className="text-sm text-muted-foreground">{t('network.editor.tunnelAddressing')}</p>
      ) : (
        <ToggleField
          title={t('network.editor.dhcp')}
          description={t('network.editor.dhcpCopy')}
          control={<Switch checked={dhcp} onCheckedChange={setDhcp} aria-label={t('network.editor.dhcp')} />}
        />
      )}
      {addressing === 'static' ? (
        <>
          <FormField label={t('network.editor.address')}>
            {(id) => <Input id={id} className="font-mono" value={address} onChange={(event) => setAddress(event.target.value)} placeholder="192.168.1.20/24" required />}
          </FormField>
          {kind === 'wireguard' ? null : (
            <>
              <FormField label={t('network.editor.gateway')}>
                {(id) => <Input id={id} className="font-mono" value={gateway} onChange={(event) => setGateway(event.target.value)} />}
              </FormField>
              <FormField label={t('network.editor.dns')}>
                {(id) => <Input id={id} className="font-mono" value={dns} onChange={(event) => setDns(event.target.value)} />}
              </FormField>
            </>
          )}
        </>
      ) : null}
      {kind === 'vlan' ? (
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={t('network.editor.parent')} hint={candidates.length === 0 ? t('network.editor.noPorts') : undefined}>
            {(id) => (
              <Select value={parentValue} onValueChange={(value) => setParent(String(value))}>
                <SelectTrigger id={id} aria-label={t('network.editor.parent')}><SelectValue /></SelectTrigger>
                <SelectContent>{candidates.map((name) => <SelectItem value={name} key={name}>{name}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
          <FormField label={t('network.editor.vlanId')}>
            {(id) => <Input id={id} type="number" min={1} max={4094} value={vlanId} onChange={(event) => setVlanId(event.target.value)} required />}
          </FormField>
        </div>
      ) : null}
      {kind === 'bridge' ? (
        <FormField label={t('network.editor.ports')} hint={candidates.length === 0 ? t('network.editor.noPorts') : t('network.editor.portsHint')}>
          {/* A group and not a single control: the label names the set, and
              each port carries its own checkbox label. */}
          {(id) => (
            <div id={id} role="group" aria-label={t('network.editor.ports')} className="grid gap-2 sm:grid-cols-2">
              {candidates.map((name) => (
                <label key={name} className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={ports.includes(name)}
                    onChange={(event) => setPorts(event.target.checked ? [...ports, name] : ports.filter((port) => port !== name))}
                  />
                  <span className="font-mono">{name}</span>
                </label>
              ))}
            </div>
          )}
        </FormField>
      ) : null}
      {kind === 'wireguard' ? (
        <FormField label={t('network.editor.listenPort')}>
          {(id) => <Input id={id} type="number" min={1} max={65535} value={listenPort} onChange={(event) => setListenPort(event.target.value)} />}
        </FormField>
      ) : null}
    </FormDialog>
  )
}

function WifiPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [adding, setAdding] = useState(false)
  const [editing, setEditing] = useState<WifiNetwork>()
  const [ssid, setSsid] = useState('')
  const [psk, setPsk] = useState('')
  const [hidden, setHidden] = useState(false)
  const [priority, setPriority] = useState('0')
  const [radio, setRadio] = useState<string>()
  // The station role, as its own resource: one radio, one switch. The list
  // beside it is a separate collection because it carries keys.
  const client = useQuery({ queryKey: ['wifi-client'], queryFn: () => api<WifiClientRole>('/api/v1/wifi/client') })
  const status = useQuery({ queryKey: ['observed-network'], queryFn: () => api<ObservedNetworkState>('/api/v1/network/status'), retry: false })
  const networks = useQuery({ queryKey: ['wifi-networks'], queryFn: () => api<WifiNetwork[]>('/api/v1/wifi/client/networks') })
  const radios = status.data?.capabilities.wifi.interfaces ?? []
  const currentRadio = radio ?? client.data?.interface ?? radios[0] ?? ''
  const writeRole = useMutationFeedback<TaskAccepted, WifiClientRole>({
    mutationFn: (role) => api<TaskAccepted>('/api/v1/wifi/client', json('PUT', role)),
    success: (_data, role) => t(role.enabled ? 'network.wifi.clientEnabled' : 'network.wifi.clientDisabled'),
    failure: t('network.wifi.client'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wifi-client'] }),
  })
  const toggle = {
    data: writeRole.data,
    mutate: (enabled: boolean) => writeRole.mutate({ enabled, interface: currentRadio }),
  }
  const remove = useMutationFeedback<void, string>({
    mutationFn: (name) => api<void>(`/api/v1/wifi/client/networks/${encodeURIComponent(name)}`, { method: 'DELETE' }),
    success: (_data, name) => t('network.wifi.removed', { name }),
    failure: t('network.wifi.remove', { name: '' }),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wifi-networks'] }),
  })
  const addNetwork = () => api<WifiNetwork>('/api/v1/wifi/client/networks', json('POST', { ssid, ...(psk ? { psk } : {}), hidden, priority: Number(priority) }))
    .then(() => {
      // Every field resets, not just the two that used to: a reopened dialog
      // showing the previous network's priority is a value nobody chose.
      setSsid(''); setPsk(''); setHidden(false); setPriority('0')
      return queryClient.invalidateQueries({ queryKey: ['wifi-networks'] })
    })
  // A scan is a POST: it sweeps every channel and briefly costs the station
  // its link, so it happens when an operator asks and never on a render.
  const scan = useMutationFeedback<WifiScan>({
    mutationFn: () => api<WifiScan>('/api/v1/wifi/client/scan', { method: 'POST' }),
    success: t('network.wifi.scanned'),
    failure: t('network.wifi.scan'),
  })

  const connect = (network: NonNullable<WifiScan['networks']>[number]) => {
    setSsid(network.ssid)
    setPsk('')
    setHidden(network.ssid === '')
    setPriority('0')
    setAdding(true)
  }

  const editNetwork = () => {
    const entry = editing
    if (!entry) return Promise.resolve()
    return api<WifiNetwork>(`/api/v1/wifi/client/networks/${encodeURIComponent(entry.ssid)}`, json('PUT', {
      ssid: entry.ssid,
      // Absent keeps the stored key: the operator was never shown it, so an
      // empty box is "leave it alone" and never "make this an open network".
      ...(psk ? { psk } : {}),
      hidden,
      priority: Number(priority),
    })).then(() => {
      setEditing(undefined); setPsk('')
      return queryClient.invalidateQueries({ queryKey: ['wifi-networks'] })
    })
  }
  const startEditing = (entry: WifiNetwork) => {
    setEditing(entry); setPsk(''); setHidden(entry.hidden); setPriority(entry.priority.toString())
  }
  const clientState = client.isPending ? 'common.states.pending' : client.isError ? 'common.states.unknown' : client.data?.enabled ? 'common.states.enabled' : 'common.states.disabled'
  const panelError = client.error ?? networks.error

  return (
    <>
      <div className="flex flex-wrap items-center gap-3">
        <StatusBadge tone={client.isPending || client.isError ? 'warning' : client.data?.enabled ? 'success' : 'neutral'}>{t(clientState)}</StatusBadge>
        {radios.length > 1 ? (
          <Select value={currentRadio} onValueChange={(value) => { const next = String(value); setRadio(next); writeRole.mutate({ enabled: client.data?.enabled ?? false, interface: next }) }}>
            <SelectTrigger className="w-40" aria-label={t('network.wifi.radio')}><SelectValue /></SelectTrigger>
            <SelectContent>{radios.map((name) => <SelectItem value={name} key={name}>{name}</SelectItem>)}</SelectContent>
          </Select>
        ) : (
          <span className="font-mono text-sm text-muted-foreground" aria-label={t('network.wifi.radio')}>{currentRadio || t('common.notAvailable')}</span>
        )}
        <div className="ml-auto flex items-center gap-3">
          <Switch checked={client.data?.enabled ?? false} onCheckedChange={(value) => toggle.mutate(value)} aria-label={t('network.wifi.client')} />
          <Button size="sm" variant="outline" onClick={() => setAdding(true)}><Plus />{t('network.wifi.add')}</Button>
        </div>
      </div>
      {panelError ? <Callout tone="danger" title={failureDetail(panelError, t('common.requestFailed'))} /> : null}
      <CollectionPanel
        title={t('network.wifi.scanTitle')}
        action={(
          <Button size="sm" variant="outline" onClick={() => scan.mutate()} disabled={scan.isPending}>
            <Radar />{scan.isPending ? t('network.wifi.scanning') : t('network.wifi.scan')}
          </Button>
        )}
      >
        {scan.data && scan.data.available === false ? <Callout tone="warning" title={scan.data.detail ?? t('network.wifi.scanUnavailable')} /> : null}
        <DataTable<NonNullable<WifiScan['networks']>[number]>
          rows={scan.data?.networks}
          rowKey={(entry) => entry.bssid}
          isPending={scan.isPending}
          empty={t('network.wifi.scanEmpty')}
          columns={[
            { id: 'ssid', header: 'SSID', cell: (entry) => <span className="font-mono">{entry.ssid || t('network.wifi.hiddenNetwork')}</span> },
            { id: 'signal', header: t('network.wifi.signal'), cell: (entry) => entry.signalDbm === undefined ? '—' : `${entry.signalDbm} dBm` },
            { id: 'security', header: t('network.wifi.security'), cell: (entry) => entry.flags.includes('PSK') ? t('network.wifi.wpa') : t('network.wifi.open') },
            { id: 'actions', header: '', align: 'end', cell: (entry) => (
              <Button type="button" size="sm" variant="outline" onClick={() => connect(entry)}>{t('network.wifi.connect')}</Button>
            ) },
          ]}
        />
      </CollectionPanel>
      <CollectionPanel>
        <DataTable<WifiNetwork>
          rows={networks.data}
          rowKey={(entry) => entry.ssid}
          isPending={networks.isPending}
          empty={t('network.wifi.empty')}
          columns={[
            { id: 'ssid', header: 'SSID', cell: (entry) => <span className="font-mono">{entry.ssid}{entry.hidden ? <span className="text-muted-foreground"> · {t('network.wifi.hidden')}</span> : null}</span> },
            { id: 'security', header: t('network.wifi.security'), cell: (entry) => t(entry.psk ? 'network.wifi.wpa' : 'network.wifi.open') },
            { id: 'credential', header: t('network.wifi.credential'), cell: (entry) => <StatusBadge>{t(entry.psk ? 'network.wifi.saved' : 'network.wifi.noCredential')}</StatusBadge> },
            { id: 'auto', header: t('network.wifi.auto'), cell: (entry) => t('network.wifi.priorityValue', { priority: entry.priority }) },
            { id: 'actions', header: '', align: 'end', cell: (entry) => (
              <span className="flex justify-end gap-2">
              <Button type="button" size="sm" variant="outline" onClick={() => startEditing(entry)}>{t('common.actions.edit')}</Button>
              <ConfirmDialog
                trigger={<Button type="button" size="sm" variant="destructive" aria-label={t('network.wifi.remove', { name: entry.ssid })}><Trash2 />{t('network.wifi.remove', { name: entry.ssid })}</Button>}
                title={t('network.wifi.remove', { name: entry.ssid })}
                description={t('network.wifi.removeCopy', { name: entry.ssid })}
                confirmLabel={t('network.wifi.remove', { name: entry.ssid })}
                success={t('network.wifi.removed', { name: entry.ssid })}
                failure={t('network.wifi.remove', { name: entry.ssid })}
                onConfirm={() => remove.mutateAsync(entry.ssid)}
              />
              </span>
            ) },
          ]}
        />
      </CollectionPanel>
      <WifiApPanel />
      <TaskProgress taskId={toggle.data?.taskId} />
      <FormDialog
        open={adding}
        onOpenChange={setAdding}
        title={t('network.wifi.add')}
        description={t('network.wifi.addCopy')}
        submitLabel={t('common.actions.add')}
        success={t('network.wifi.added', { name: ssid })}
        failure={t('network.wifi.add')}
        onSubmit={addNetwork}
      >
        <FormField label="SSID">
          {(id) => <Input id={id} value={ssid} onChange={(event) => setSsid(event.target.value)} required />}
        </FormField>
        <FormField label={t('network.wifi.password')}>
          {(id) => <Input id={id} type="password" minLength={8} value={psk} onChange={(event) => setPsk(event.target.value)} />}
        </FormField>
        <FormField label={t('network.wifi.priority')}>
          {(id) => <Input id={id} type="number" value={priority} onChange={(event) => setPriority(event.target.value)} />}
        </FormField>
        <ToggleField title={t('network.wifi.hidden')} control={<Switch checked={hidden} onCheckedChange={setHidden} aria-label={t('network.wifi.hidden')} />} />
      </FormDialog>
      <FormDialog
        open={editing !== undefined}
        onOpenChange={(next) => { if (!next) setEditing(undefined) }}
        title={t('network.wifi.edit', { name: editing?.ssid ?? '' })}
        description={t('network.wifi.editCopy')}
        submitLabel={t('common.actions.save')}
        success={t('network.wifi.added', { name: editing?.ssid ?? '' })}
        failure={t('network.wifi.edit', { name: editing?.ssid ?? '' })}
        onSubmit={editNetwork}
      >
        <FormField label={t('network.wifi.password')} hint={t('network.wifi.keepKey')}>
          {(id) => <Input id={id} type="password" minLength={8} value={psk} onChange={(event) => setPsk(event.target.value)} placeholder={t('network.wifi.keepKeyPlaceholder')} />}
        </FormField>
        <FormField label={t('network.wifi.priority')}>
          {(id) => <Input id={id} type="number" value={priority} onChange={(event) => setPriority(event.target.value)} />}
        </FormField>
        <ToggleField title={t('network.wifi.hidden')} control={<Switch checked={hidden} onCheckedChange={setHidden} aria-label={t('network.wifi.hidden')} />} />
      </FormDialog>
    </>
  )
}

function WireguardPanel({ configured }: { configured: Record<string, InterfaceConfig> }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const tunnels = useMemo(() => Object.entries(configured).filter(([, value]) => value.kind === 'wireguard'), [configured])
  const [selected, setSelected] = useState(tunnels[0]?.[0] ?? '')
  const iface = selected || tunnels[0]?.[0] || ''
  const tunnel = configured[iface]
  const [adding, setAdding] = useState(false)
  const [publicKey, setPublicKey] = useState('')
  const [allowedIps, setAllowedIps] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const peers = useQuery({ queryKey: ['wireguard-peers', iface], queryFn: () => api<WireguardPeer[]>(`/api/v1/network/${encodeURIComponent(iface)}/peers`), enabled: Boolean(iface) })
  // The public half of the key micad generated on the device. It is published
  // into live state by the network reconciler and nowhere else: the private
  // key never enters the settings tree, so this is the only place a remote
  // peer's configuration can be read from.
  const state = useQuery({ queryKey: ['state', 'network'], queryFn: () => api<Record<string, { publicKey?: string }>>('/api/v1/state/network'), retry: false })
  const remove = useMutationFeedback<void, string>({
    mutationFn: (key) => api<void>(`/api/v1/network/${encodeURIComponent(iface)}/peers/${encodeURIComponent(key)}`, { method: 'DELETE' }),
    success: t('network.wireguard.removed'),
    failure: t('network.wireguard.remove'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wireguard-peers', iface] }),
  })
  const rotate = useMutationFeedback<WireguardRotation>({
    mutationFn: () => api<WireguardRotation>(`/api/v1/actions/wireguard/${encodeURIComponent(iface)}/rotate-key`, { method: 'POST' }),
    success: t('network.wireguard.rotatedKey'),
    failure: t('network.wireguard.rotate'),
  })
  const addPeer = () => api<WireguardPeer>(`/api/v1/network/${encodeURIComponent(iface)}/peers`, json('POST', { publicKey, allowedIps: splitList(allowedIps), ...(endpoint ? { endpoint } : {}) }))
    .then(() => {
      setPublicKey(''); setAllowedIps(''); setEndpoint('')
      return queryClient.invalidateQueries({ queryKey: ['wireguard-peers', iface] })
    })

  // The rotation's answer first: it is newer than the live state the page last
  // read, and an operator who just rotated is looking at this card.
  const devicePublicKey = rotate.data?.publicKey ?? state.data?.[iface]?.publicKey
  // The block a remote peer needs, in wg-quick's own spelling. `AllowedIPs` is
  // this device's tunnel address: what the remote should route here, and the
  // one value that cannot be guessed from the key.
  const localPeerConfig = [
    '[Peer]',
    `PublicKey = ${devicePublicKey ?? ''}`,
    `AllowedIPs = ${tunnel?.static?.address ?? ''}`,
    ...(tunnel?.wireguard?.listenPort ? [`Endpoint = ${window.location.hostname}:${tunnel.wireguard.listenPort}`] : []),
  ].join('\n')

  if (tunnels.length === 0) {
    return <Panel><p className="flex items-center justify-center gap-2 py-6 text-sm text-muted-foreground"><KeyRound className="size-4" />{t('network.wireguard.empty')}</p></Panel>
  }
  return (
    <>
      {tunnels.length > 1 ? (
        <Select value={iface} onValueChange={(value) => { if (value) setSelected(value) }}>
          <SelectTrigger className="w-56" aria-label={t('network.wireguard.tunnel')}><SelectValue /></SelectTrigger>
          <SelectContent>{tunnels.map(([name]) => <SelectItem value={name} key={name}>{name}</SelectItem>)}</SelectContent>
        </Select>
      ) : null}
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <MetricCard label={t('network.wireguard.tunnel')} mono value={`${iface} · ${tunnel?.static?.address ?? t('common.notAvailable')}`} caption={peers.data?.length ? t('network.wireguard.peerCount', { count: peers.data.length }) : t('network.wireguard.noPeers')} />
        <MetricCard label={t('network.wireguard.listen')} mono value={tunnel?.wireguard?.listenPort ? `${tunnel.wireguard.listenPort}/udp` : t('common.notAvailable')} caption={t('network.wireguard.listenCopy')} />
        <MetricCard label={t('network.wireguard.publicKey')} mono value={<span className="text-sm break-all">{devicePublicKey ?? t('network.wireguard.keyHidden')}</span>} caption={t('network.wireguard.keyCopy')} />
      </div>
      {devicePublicKey ? (
        <Panel title={t('network.wireguard.localTitle')} description={t('network.wireguard.localCopy')}>
          <CopyField value={localPeerConfig} label={t('common.actions.copy')} />
        </Panel>
      ) : null}
      {peers.error ? <Callout tone="danger" title={failureDetail(peers.error, t('common.requestFailed'))} /> : null}
      <CollectionPanel
        title={t('network.wireguard.peers')}
        action={<Button size="sm" variant="outline" onClick={() => setAdding(true)}><Plus />{t('network.wireguard.add')}</Button>}
      >
        <DataTable<WireguardPeer>
          rows={peers.data}
          rowKey={(peer) => peer.publicKey}
          isPending={peers.isPending}
          empty={t('network.wireguard.noPeers')}
          columns={[
            { id: 'key', header: t('network.wireguard.publicKey'), cell: (peer) => <span className="font-mono text-[0.8125rem] break-all">{peer.publicKey}</span> },
            { id: 'allowed', header: t('network.wireguard.allowed'), cell: (peer) => <span className="font-mono text-[0.8125rem]">{peer.allowedIps.join(', ')}</span> },
            { id: 'endpoint', header: t('network.wireguard.endpoint'), cell: (peer) => <span className="font-mono text-[0.8125rem]">{peer.endpoint ?? '—'}</span> },
            { id: 'keepalive', header: t('network.wireguard.keepalive'), cell: (peer) => peer.persistentKeepalive ? t('network.wireguard.keepaliveValue', { seconds: peer.persistentKeepalive }) : '—' },
            { id: 'actions', header: '', align: 'end', cell: (peer) => (
              <ConfirmDialog
                trigger={<Button type="button" size="sm" variant="destructive" aria-label={t('network.wireguard.remove')}><Trash2 />{t('network.wireguard.remove')}</Button>}
                title={t('network.wireguard.remove')}
                description={t('network.wireguard.removeCopy')}
                confirmLabel={t('network.wireguard.remove')}
                success={t('network.wireguard.removed')}
                failure={t('network.wireguard.remove')}
                onConfirm={() => remove.mutateAsync(peer.publicKey)}
              />
            ) },
          ]}
        />
      </CollectionPanel>
      <Panel className="border-destructive">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="grid gap-0.5">
            <strong className="text-sm font-medium">{t('network.wireguard.rotate')}</strong>
            <span className="text-sm text-muted-foreground">{t('network.wireguard.rotateCopy')}</span>
          </div>
          <ConfirmDialog
            trigger={<Button type="button" size="sm" variant="destructive">{t('network.wireguard.rotate')}</Button>}
            title={t('network.wireguard.rotate')}
            description={t('network.wireguard.rotateConfirm')}
            confirmLabel={t('network.wireguard.rotate')}
            success={t('network.wireguard.rotatedKey')}
            failure={t('network.wireguard.rotate')}
            onConfirm={() => rotate.mutateAsync()}
          />
        </div>
      </Panel>
      <FormDialog
        open={adding}
        onOpenChange={setAdding}
        title={t('network.wireguard.add')}
        description={t('network.wireguard.addCopy', { iface })}
        submitLabel={t('common.actions.add')}
        success={t('network.wireguard.added')}
        failure={t('network.wireguard.add')}
        onSubmit={addPeer}
      >
        <FormField label={t('network.wireguard.publicKey')}>
          {(id) => <Input id={id} className="font-mono" value={publicKey} onChange={(event) => setPublicKey(event.target.value)} required />}
        </FormField>
        <FormField label={t('network.wireguard.allowed')}>
          {(id) => <Input id={id} className="font-mono" value={allowedIps} onChange={(event) => setAllowedIps(event.target.value)} placeholder="10.10.0.0/24" required />}
        </FormField>
        <FormField label={t('network.wireguard.endpoint')}>
          {(id) => <Input id={id} className="font-mono" value={endpoint} onChange={(event) => setEndpoint(event.target.value)} placeholder="vpn.example.com:51820" />}
        </FormField>
      </FormDialog>
    </>
  )
}

function summarize(items: unknown[] | undefined) {
  if (!items?.length) return '—'
  return items.map((item) => {
    if (typeof item === 'string') return item
    if (item && typeof item === 'object') {
      const value = item as Record<string, unknown>
      const address = value.Address ?? value.address
      const prefix = value.PrefixLength ?? value.prefixLength
      if (Array.isArray(address)) return `${address.join('.')}${prefix === undefined ? '' : `/${prefix}`}`
      if (typeof address === 'string') return `${address}${prefix === undefined ? '' : `/${prefix}`}`
    }
    return 'address'
  }).join(', ')
}

function splitList(value: string) { return value.split(',').map((item) => item.trim()).filter(Boolean) }
