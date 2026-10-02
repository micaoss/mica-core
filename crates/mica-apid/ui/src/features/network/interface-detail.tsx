import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Check, ChevronLeft } from 'lucide-react'
import { api, json } from '@/shared/lib/http'
import type { Health, NetworkOverview, ObservedNetworkInterface, ObservedNetworkState, TaskAccepted, TaskRecord } from '@/lib/types'
import { FactList } from '@/shared/components/fact-list'
import { FormDialog } from '@/shared/components/form-dialog'
import { FormField } from '@/shared/components/form-field'
import { Page, PageHeader, PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { StatusBadge } from '@/shared/components/status-badge'
import { Button, buttonVariants } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { connectionState } from '@/features/shell/connection'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { networkRows, physicalInterfaces } from '@/lib/network'
import { applyStage, applyStages, stageState } from './apply-stage'
import { ObservedInterfaceFacts } from './observed-network-panel'
import { bridgeMembership, isSessionInterface } from './interface-facts'

interface StaticRoute { destination: string; gateway?: string; metric?: number }
interface DhcpServer { poolOffset: number; poolSize: number; dns: string[]; leaseSeconds?: number }

interface InterfaceConfig {
  kind?: 'physical' | 'vlan' | 'bridge' | 'wireguard'
  dhcp: boolean
  static?: { address: string; gateway?: string; dns: string[] }
  vlan?: { parent: string; id: number }
  bridge?: { ports: string[] }
  wireguard?: { listenPort?: number; peers: unknown[] }
  routes?: StaticRoute[]
  dhcpServer?: DhcpServer
}

export function InterfaceDetailPage() {
  const { name } = useParams({ from: '/network_/$name' })
  const { t } = useTranslation()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const network = useQuery({ queryKey: ['network'], queryFn: () => api<NetworkOverview>('/api/v1/network'), refetchInterval: 10_000 })
  const status = useQuery({ queryKey: ['observed-network'], queryFn: () => api<ObservedNetworkState>('/api/v1/network/status'), refetchInterval: 15_000, retry: false })
  const configuredMap = (network.data?.configured ?? {}) as Record<string, InterfaceConfig>
  const configured = configuredMap[name]
  const observed = status.data?.interfaces.entries?.find((entry) => entry.name === name)
  const bridge = bridgeMembership(configuredMap, name)

  const [dhcp, setDhcp] = useState<boolean | undefined>()
  const [address, setAddress] = useState<string | undefined>()
  const [gateway, setGateway] = useState<string | undefined>()
  const [dns, setDns] = useState<string | undefined>()
  const [parent, setParent] = useState<string | undefined>()
  const [vlanId, setVlanId] = useState<string | undefined>()
  const [ports, setPorts] = useState<string[] | undefined>()
  const [listenPort, setListenPort] = useState<string | undefined>()
  const [routes, setRoutes] = useState<StaticRoute[] | undefined>()
  const [server, setServer] = useState<DhcpServer | undefined>()
  const [review, setReview] = useState(false)
  const [taskId, setTaskId] = useState<string>()

  const kind = configured?.kind ?? 'physical'
  // A tunnel has no DHCP client: its addressing is the address on the link.
  const useDhcp = kind === 'wireguard' ? false : dhcp ?? configured?.dhcp ?? true
  const addressValue = address ?? configured?.static?.address ?? ''
  const gatewayValue = gateway ?? configured?.static?.gateway ?? ''
  const dnsValue = dns ?? configured?.static?.dns.join(', ') ?? ''
  const candidates = physicalInterfaces(networkRows(configuredMap, network.data?.observed.interfaces))
    .filter((candidate) => candidate !== name)
  const parentValue = parent ?? configured?.vlan?.parent ?? candidates[0] ?? ''
  const vlanIdValue = vlanId ?? (configured?.vlan?.id ?? 100).toString()
  const portsValue = ports ?? configured?.bridge?.ports ?? []
  const listenPortValue = listenPort ?? (configured?.wireguard?.listenPort ?? 51820).toString()
  const routeRows = routes ?? configured?.routes ?? []
  const serverRow = server ?? configured?.dhcpServer
  // A server needs a subnet of its own to hand addresses out of, and a port
  // has no addressing at all. Both are the device's rules, stated here so the
  // form does not offer what the device would refuse.
  const canServe = !useDhcp && addressValue !== '' && bridge === undefined

  const save = useMutation({
    mutationFn: () => {
      const value: InterfaceConfig = { ...configured, dhcp: useDhcp }
      if (useDhcp) delete value.static
      else {
        value.static = {
          address: addressValue,
          // A tunnel declares neither: where its traffic goes is each peer's
          // allowed IPs, and micad refuses an entry that claims otherwise.
          ...(gatewayValue && kind !== 'wireguard' ? { gateway: gatewayValue } : {}),
          dns: kind === 'wireguard' ? [] : splitList(dnsValue),
        }
      }
      if (kind === 'vlan') value.vlan = { parent: parentValue, id: Number(vlanIdValue) }
      if (kind === 'bridge') value.bridge = { ports: portsValue }
      if (kind === 'wireguard') {
        value.wireguard = { ...configured?.wireguard, listenPort: Number(listenPortValue), peers: configured?.wireguard?.peers ?? [] }
      }
      // Absent and empty are the same thing to the device, and an entry that
      // declares neither is byte-identical to one written before either
      // existed.
      if (routeRows.length > 0) value.routes = routeRows
      else delete value.routes
      if (serverRow && canServe) value.dhcpServer = serverRow
      else delete value.dhcpServer
      return api<TaskAccepted>(`/api/v1/network/${encodeURIComponent(name)}`, json('PUT', value))
    },
    onSuccess: (accepted) => {
      setReview(false)
      setTaskId(accepted.taskId)
      void queryClient.invalidateQueries({ queryKey: ['network'] })
    },
  })

  const remove = useMutation({
    mutationFn: () => api<void>(`/api/v1/network/${encodeURIComponent(name)}`, { method: 'DELETE' }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['network'] })
      void navigate({ to: '/network' })
    },
  })

  const reset = () => {
    setDhcp(undefined); setAddress(undefined); setGateway(undefined); setDns(undefined)
    setParent(undefined); setVlanId(undefined); setPorts(undefined); setListenPort(undefined)
    setRoutes(undefined); setServer(undefined)
  }
  const submit = (event: FormEvent) => { event.preventDefault(); setReview(true) }
  const link = observed?.link
  const online = link?.carrier === true
  const onSession = isSessionInterface((observed?.addresses ?? []).map(formatAddress), window.location.hostname)

  return (
    <Page>
      <PageHeader
        title={name}
        back={<Link to="/network" className={buttonVariants({ variant: 'outline', size: 'sm' })}><ChevronLeft aria-hidden="true" />{t('network.title')} / {t('network.tabs.interfaces')}</Link>}
        action={(
          <>
            <StatusBadge>{kindLabel(configured, observed, t)}</StatusBadge>
            <StatusBadge tone={online ? 'success' : 'danger'}>{link?.operationalState ?? t('network.notObserved')}</StatusBadge>
          </>
        )}
      />

      {taskId ? <ApplyStrip taskId={taskId} onDismiss={() => setTaskId(undefined)} /> : null}

      <PageSection title={t('network.detail.overview')}>
        <Panel>
          <FactList facts={[
            { id: 'mac', label: t('network.detail.mac'), value: observed?.hardwareAddress ?? t('common.notAvailable'), mono: true },
            { id: 'mtu', label: t('network.detail.mtu'), value: observed?.mtu ?? t('common.notAvailable'), mono: true },
            { id: 'bridge', label: t('network.detail.memberOf'), value: bridge ?? t('network.detail.noBridge'), mono: true },
            { id: 'link', label: t('network.detail.link'), value: <span className={online ? undefined : 'text-destructive'}>{link?.carrierState ?? t('common.notAvailable')}</span> },
          ]} />
        </Panel>
      </PageSection>

      <PageSection title={t('network.observed.title')} description={t('network.detail.observedCopy')}>
        <Panel>
          {observed
            ? <ObservedInterfaceFacts value={observed} />
            : <p className="py-4 text-center text-sm text-muted-foreground">{t('network.notObserved')}</p>}
        </Panel>
      </PageSection>

      <PageSection title={t('network.detail.addressing')}>
        <Panel>
          <form className="grid gap-4" onSubmit={submit}>
            {kind === 'wireguard' ? (
              <p className="text-sm text-muted-foreground">{t('network.editor.tunnelAddressing')}</p>
            ) : (
              <FormField label={t('network.detail.mode')}>
                {() => (
                  <SegmentedControl<'dhcp' | 'static'>
                    label={t('network.detail.mode')}
                    value={useDhcp ? 'dhcp' : 'static'}
                    onValueChange={(mode) => setDhcp(mode === 'dhcp')}
                    segments={[
                      { value: 'dhcp', label: 'DHCP' },
                      { value: 'static', label: t('network.detail.static') },
                    ]}
                  />
                )}
              </FormField>
            )}
            {useDhcp ? <p className="text-sm text-muted-foreground">{t('network.detail.dhcpNote')}</p> : (
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField label={t('network.detail.address')}>
                  {(id) => <Input id={id} className="font-mono" value={addressValue} onChange={(event) => setAddress(event.target.value)} placeholder="10.0.0.2/24" required />}
                </FormField>
                {kind === 'wireguard' ? null : (
                  <>
                    <FormField label={t('network.detail.gateway')}>
                      {(id) => <Input id={id} className="font-mono" value={gatewayValue} onChange={(event) => setGateway(event.target.value)} placeholder="10.0.0.1" />}
                    </FormField>
                    <FormField className="sm:col-span-2" label={t('network.detail.dns')}>
                      {(id) => <Input id={id} className="font-mono" value={dnsValue} onChange={(event) => setDns(event.target.value)} placeholder="10.0.0.1, 1.1.1.1" />}
                    </FormField>
                  </>
                )}
              </div>
            )}
            {kind === 'vlan' ? (
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField label={t('network.editor.parent')}>
                  {(id) => (
                    <Select value={parentValue} onValueChange={(value) => setParent(String(value))}>
                      <SelectTrigger id={id} aria-label={t('network.editor.parent')}><SelectValue /></SelectTrigger>
                      <SelectContent>{candidates.map((candidate) => <SelectItem value={candidate} key={candidate}>{candidate}</SelectItem>)}</SelectContent>
                    </Select>
                  )}
                </FormField>
                <FormField label={t('network.editor.vlanId')}>
                  {(id) => <Input id={id} type="number" min={1} max={4094} value={vlanIdValue} onChange={(event) => setVlanId(event.target.value)} required />}
                </FormField>
              </div>
            ) : null}
            {kind === 'bridge' ? (
              <FormField label={t('network.editor.ports')} hint={t('network.editor.portsHint')}>
                {(id) => (
                  <div id={id} role="group" aria-label={t('network.editor.ports')} className="grid gap-2 sm:grid-cols-2">
                    {candidates.map((candidate) => (
                      <label key={candidate} className="flex items-center gap-2 text-sm">
                        <input
                          type="checkbox"
                          className="size-4 accent-primary"
                          checked={portsValue.includes(candidate)}
                          onChange={(event) => setPorts(event.target.checked ? [...portsValue, candidate] : portsValue.filter((port) => port !== candidate))}
                        />
                        <span className="font-mono">{candidate}</span>
                      </label>
                    ))}
                  </div>
                )}
              </FormField>
            ) : null}
            {kind === 'wireguard' ? (
              <FormField label={t('network.editor.listenPort')} hint={t('network.editor.tunnelAddressing')}>
                {(id) => <Input id={id} type="number" min={1} max={65535} value={listenPortValue} onChange={(event) => setListenPort(event.target.value)} />}
              </FormField>
            ) : null}
            {bridge === undefined ? (
              <fieldset className="grid gap-3">
                <legend className="text-sm font-medium">{t('network.routes.title')}</legend>
                <p className="text-sm text-muted-foreground">{t('network.routes.copy')}</p>
                {routeRows.length === 0 ? <p className="text-sm text-muted-foreground">{t('network.routes.empty')}</p> : null}
                {routeRows.map((route, index) => (
                  <div className="grid items-end gap-3 sm:grid-cols-[1fr_1fr_auto_auto]" key={index}>
                    <FormField label={t('network.routes.destination')}>
                      {(id) => <Input id={id} className="font-mono" value={route.destination} placeholder="10.20.0.0/16" onChange={(event) => setRoutes(routeRows.map((row, at) => at === index ? { ...row, destination: event.target.value } : row))} required />}
                    </FormField>
                    <FormField label={t('network.routes.via')}>
                      {(id) => <Input id={id} className="font-mono" value={route.gateway ?? ''} placeholder={t('network.routes.onLink')} onChange={(event) => setRoutes(routeRows.map((row, at) => at === index ? { ...row, gateway: event.target.value || undefined } : row))} />}
                    </FormField>
                    <FormField label={t('network.routes.metric')}>
                      {(id) => <Input id={id} className="sm:w-24" type="number" min={0} value={route.metric ?? ''} onChange={(event) => setRoutes(routeRows.map((row, at) => at === index ? { ...row, metric: event.target.value === '' ? undefined : Number(event.target.value) } : row))} />}
                    </FormField>
                    <Button type="button" variant="outline" onClick={() => setRoutes(routeRows.filter((_, at) => at !== index))}>{t('common.actions.delete')}</Button>
                  </div>
                ))}
                <Button className="justify-self-start" type="button" variant="secondary" onClick={() => setRoutes([...routeRows, { destination: '' }])}>{t('network.routes.add')}</Button>
              </fieldset>
            ) : null}
            {canServe ? (
              <fieldset className="grid gap-3">
                <legend className="text-sm font-medium">{t('network.dhcpServer.title')}</legend>
                <p className="text-sm text-muted-foreground">{t('network.dhcpServer.copy')}</p>
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={serverRow !== undefined}
                    onChange={(event) => setServer(event.target.checked ? { poolOffset: 100, poolSize: 50, dns: [] } : undefined)}
                  />
                  {t('network.dhcpServer.enable')}
                </label>
                {serverRow ? (
                  <div className="grid gap-4 sm:grid-cols-2">
                    <FormField label={t('network.dhcpServer.poolOffset')} hint={t('network.dhcpServer.poolHint')}>
                      {(id) => <Input id={id} type="number" min={1} value={serverRow.poolOffset} onChange={(event) => setServer({ ...serverRow, poolOffset: Number(event.target.value) })} required />}
                    </FormField>
                    <FormField label={t('network.dhcpServer.poolSize')}>
                      {(id) => <Input id={id} type="number" min={1} value={serverRow.poolSize} onChange={(event) => setServer({ ...serverRow, poolSize: Number(event.target.value) })} required />}
                    </FormField>
                    <FormField label={t('network.dhcpServer.dns')} hint={t('network.dhcpServer.dnsHint')}>
                      {(id) => <Input id={id} className="font-mono" value={serverRow.dns.join(', ')} onChange={(event) => setServer({ ...serverRow, dns: splitList(event.target.value) })} />}
                    </FormField>
                    <FormField label={t('network.dhcpServer.lease')}>
                      {(id) => <Input id={id} type="number" min={1} value={serverRow.leaseSeconds ?? ''} onChange={(event) => setServer({ ...serverRow, leaseSeconds: event.target.value === '' ? undefined : Number(event.target.value) })} />}
                    </FormField>
                  </div>
                ) : null}
              </fieldset>
            ) : null}
            <div className="flex justify-end gap-2">
              <Button type="button" variant="outline" onClick={reset}>{t('common.actions.cancel')}</Button>
              <Button type="submit">{t('network.detail.save')}</Button>
            </div>
          </form>
        </Panel>
      </PageSection>

      <PageSection title={t('network.detail.danger')} tone="danger">
        <Panel className="border-destructive">
          <div className="flex flex-wrap items-center justify-between gap-4">
            <p className="max-w-xl text-sm text-muted-foreground">{t('network.detail.dangerCopy', { name })}</p>
            <ConfirmDialog
              trigger={<Button type="button" size="sm" variant="destructive">{t('network.editor.delete')}</Button>}
              title={t('network.editor.delete')}
              description={t('network.editor.deleteCopy', { name })}
              confirmLabel={t('network.editor.delete')}
              success={t('network.detail.deleted', { name })}
              failure={t('network.editor.delete')}
              onConfirm={() => remove.mutateAsync()}
            />
          </div>
        </Panel>
      </PageSection>

      <FormDialog
        open={review}
        onOpenChange={(next) => { if (!next) setReview(false) }}
        title={t('network.review.title')}
        submitLabel={t('network.review.apply')}
        success={t('network.review.applied', { name })}
        failure={t('network.detail.save')}
        onSubmit={() => save.mutateAsync()}
      >
        <FactList facts={[
          { id: 'change', label: t('network.review.change'), value: t(useDhcp ? 'network.detail.changeDhcp' : 'network.detail.changeStatic', { address: addressValue }), mono: true },
          { id: 'affects', label: t('network.review.affects'), value: bridge ? t('network.review.affectsBridge', { name, bridge }) : name },
          { id: 'session', label: t('network.review.session'), value: (
            <span className={onSession ? 'text-destructive' : 'text-success'}>
              {t(onSession ? 'network.review.sessionOn' : 'network.review.sessionOff', { name })}
            </span>
          ) },
          { id: 'recover', label: t('network.review.recover'), value: t('network.review.recoverCopy') },
        ]} />
      </FormDialog>
    </Page>
  )
}

function ApplyStrip({ taskId, onDismiss }: { taskId: string; onDismiss: () => void }) {
  const { t } = useTranslation()
  const health = useQuery({ queryKey: ['shell-health'], queryFn: () => api<Health>('/api/v1/health'), refetchInterval: 5_000 })
  const task = useQuery({
    queryKey: ['task', taskId],
    queryFn: () => api<TaskRecord>(`/api/v1/tasks/${encodeURIComponent(taskId)}`),
    refetchInterval: (query) => query.state.data?.status === 'finished' ? false : 1_000,
  })
  const connection = connectionState({ isError: health.isError, failureCount: health.failureCount, micad: health.data?.micad })
  const stage = applyStage(task.data, connection)
  const settled = stage === 'applied' || stage === 'failed'
  return (
    <Panel className="gap-3" contentClassName="gap-3">
      <div role="status" aria-live="polite" className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <strong className="text-sm font-medium">{t('network.apply.title')}</strong>
          {settled ? <StatusBadge tone={stage === 'applied' ? 'success' : 'danger'}>{t(`network.apply.${stage}`)}</StatusBadge> : null}
        </div>
        <div className="flex flex-wrap gap-5 text-sm">
          {applyStages.map((step) => {
            const state = stageState(step, stage)
            return (
              <span key={step} className={state === 'done' ? 'flex items-center gap-2 text-success' : state === 'active' ? 'flex items-center gap-2 font-semibold text-foreground' : 'flex items-center gap-2 text-muted-foreground'}>
                {state === 'done' ? <Check className="size-3.5" aria-hidden="true" /> : state === 'active' ? <Spinner className="size-3.5" /> : null}
                {t(`network.apply.steps.${step}`)}
              </span>
            )
          })}
        </div>
        <p className="text-sm text-muted-foreground">{t(`network.apply.body.${stage}`)}</p>
      </div>
      {settled ? <Button className="justify-self-start" size="sm" variant="outline" onClick={onDismiss}>{t('network.apply.dismiss')}</Button> : null}
    </Panel>
  )
}

function formatAddress(address: { address?: string; prefixLength?: number }) {
  return `${address.address ?? ''}${address.prefixLength === undefined ? '' : `/${address.prefixLength}`}`
}

function kindLabel(configured: InterfaceConfig | undefined, observed: ObservedNetworkInterface | undefined, t: ReturnType<typeof useTranslation>['t']) {
  const kind = configured?.kind ?? observed?.kind ?? observed?.type
  return kind ? t(`network.kinds.${kind}`, { defaultValue: kind }) : t('common.notAvailable')
}

function splitList(value: string) {
  return value.split(',').map((item) => item.trim()).filter(Boolean)
}
