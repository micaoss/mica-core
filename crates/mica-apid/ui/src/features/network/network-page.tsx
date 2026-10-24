import { useState } from 'react'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Callout } from '@/shared/components/callout'
import { Page, PageHeader } from '@/shared/components/page'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/shared/components/ui/tabs'
import { failureDetail } from '@/shared/feedback/toast'
import { BluetoothPanel } from './bluetooth-panel'
import { ChangeBar } from './change-bar'
import { DnsTab } from './dns-tab'
import { InterfacesTab } from './interfaces-tab'
import { OverviewTab } from './overview-tab'
import { RoutesTab } from './routes-tab'
import { useNetwork } from './use-network'
import { WifiTab } from './wifi-tab'
import { WireguardTab } from './wireguard-tab'

export const NETWORK_TABS = ['overview', 'interfaces', 'routes', 'wifi', 'dns', 'wireguard', 'bluetooth'] as const
export type NetworkTab = typeof NETWORK_TABS[number]

/// The network area: what the device runs, each interface, and the radios
/// and tunnels with their own pages. Interface changes are staged and
/// applied together from the bar at the bottom.
export function NetworkPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const search = useSearch({ from: '/network' })
  const data = useNetwork()
  // A card being edited shows its own bar in place of the change bar.
  const [cardEditing, setCardEditing] = useState(false)
  const shown = NETWORK_TABS.filter((tab) => {
    if (tab === 'wifi') return data.serves('wifi')
    if (tab === 'bluetooth') return data.serves('bluetooth')
    if (tab === 'wireguard') return !data.openrc
    return true
  })
  const tab = search.tab && shown.includes(search.tab) ? search.tab : 'interfaces'
  const select = (next: NetworkTab) => void navigate({ to: '/network', search: next === 'interfaces' ? {} : { tab: next } })

  return (
    <Page>
      <PageHeader title={t('network.title')} />
      {data.network.error ? <Callout tone="danger" title={failureDetail(data.network.error, t('common.requestFailed'))} /> : null}
      {data.status.error ? <Callout tone="warning" title={t('network.statusUnavailable')}>{failureDetail(data.status.error, t('common.requestFailed'))}</Callout> : null}
      <Tabs value={tab} onValueChange={(value) => select(value as NetworkTab)}>
        <TabsList aria-label={t('network.title')}>
          {shown.map((each) => <TabsTrigger key={each} value={each}>{t(`network.tabs.${each}`)}</TabsTrigger>)}
        </TabsList>
        <TabsContent value="overview" className="grid gap-4 pt-4"><OverviewTab data={data} onTab={select} /></TabsContent>
        <TabsContent value="interfaces" className="grid gap-4 pt-4"><InterfacesTab data={data} /></TabsContent>
        <TabsContent value="routes" className="grid gap-4 pt-4"><RoutesTab data={data} /></TabsContent>
        {data.serves('wifi') ? <TabsContent value="wifi" className="grid gap-4 pt-4"><WifiTab data={data} onEditing={setCardEditing} /></TabsContent> : null}
        <TabsContent value="dns" className="grid gap-4 pt-4"><DnsTab data={data} onEditing={setCardEditing} /></TabsContent>
        {!data.openrc ? <TabsContent value="wireguard" className="grid gap-4 pt-4"><WireguardTab data={data} tunnel={search.tunnel} onEditing={setCardEditing} /></TabsContent> : null}
        {data.serves('bluetooth') ? <TabsContent value="bluetooth" className="grid gap-4 pt-4"><BluetoothPanel onEditing={setCardEditing} /></TabsContent> : null}
      </Tabs>
      {cardEditing ? null : <ChangeBar data={data} />}
    </Page>
  )
}
