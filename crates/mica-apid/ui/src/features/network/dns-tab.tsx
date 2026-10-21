import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { FactList } from '@/shared/components/fact-list'
import { PageSection } from '@/shared/components/page'
import { Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Spinner } from '@/shared/components/ui/spinner'
import { dnsSource } from './overview-tab'
import type { NetworkData } from './use-network'

/// Which servers the device resolves with, and whether resolution works.
/// Each interface's DNS is set on its own page.
export function DnsTab({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const view = data.applied
  const groups = view.order
    .filter((name) => view.ifaces[name].kind !== 'can' && view.ifaces[name].obs)
    .map((name) => ({ name, servers: view.ifaces[name].obs?.dns ?? [], source: dnsSource(view.ifaces[name].entry) }))
    .filter((group) => group.servers.length > 0)
  const dns = data.status.data?.dns
  const probe = dns?.probe
  return (
    <PageSection title={t('network.dns.effective')} description={t('network.dns.effectiveCopy')}>
      <Panel>
        <FactList facts={[
          { id: 'mode', label: t('network.dns.mode'), value: <span><strong>{t('network.dns.perInterface')}</strong> <span className="text-muted-foreground">· {t('network.dns.perInterfaceCopy')}</span></span> },
          {
            id: 'servers',
            label: t('network.dns.servers'),
            value: groups.length ? (
              <span className="grid justify-items-end gap-0.5">
                {groups.map((group) => (
                  <span key={group.name}>
                    <Link className="font-mono text-primary" to="/network/$name" params={{ name: group.name }}>{group.name}</Link>
                    : <span className="font-mono">{group.servers.join(', ')}</span> <span className="text-muted-foreground">({t(`network.dns.sources.${group.source}`)})</span>
                  </span>
                ))}
              </span>
            ) : <span className="text-muted-foreground">{t('network.values.none')}</span>,
          },
          dns?.resolverServers?.length ? { id: 'resolver', label: t('network.dns.resolver'), value: dns.resolverServers.join(', '), mono: true } : null,
          {
            id: 'probe',
            label: t('network.dns.probe'),
            value: (
              <span className="flex flex-wrap items-center justify-end gap-2">
                {probe ? <span className={probe.reachable ? 'text-success' : 'text-destructive'}>{t(probe.reachable ? 'network.dns.resolves' : 'network.dns.unreachable')}</span> : '—'}
                {probe ? <span className="font-mono text-muted-foreground">{probe.name}</span> : null}
                <Button size="sm" variant="outline" disabled={data.status.isFetching} onClick={() => void data.status.refetch()}>
                  {data.status.isFetching ? <Spinner /> : null}{t('network.dns.recheck')}
                </Button>
              </span>
            ),
          },
        ]} />
      </Panel>
    </PageSection>
  )
}
