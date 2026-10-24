import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { Pencil, Plus, X } from 'lucide-react'
import { DataTable } from '@/shared/components/data-table'
import { FormField } from '@/shared/components/form-field'
import { Badge } from '@/shared/components/ui/badge'
import { Button } from '@/shared/components/ui/button'
import { Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/shared/components/ui/dialog'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { updateChangeSet } from './change-set'
import { Section, Tag } from './parts'
import { draftOf, draftProblems, routeTargets, stageRoute, staticRoutes, unstageRoute, type RouteDraft, type StaticRow } from './routes'
import type { NetworkData } from './use-network'
import { exitOf, routeTable, type RouteRow } from './view'

/// The routes the device runs, and the static routes of every interface,
/// edited into the staged change set.
export function RoutesTab({ data }: { data: NetworkData }) {
  return (
    <>
      <RouteTableCard data={data} />
      <StaticRoutesCard data={data} />
    </>
  )
}

/// Routes the table shows before it is unfolded.
const ROUTES_FOLDED = 20

/// The main table's IPv4 routes as the device reports them, never staged ones.
function RouteTableCard({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [all, setAll] = useState(false)
  const view = data.applied
  const exit = view.routes[0]
  const routes = routeTable(view)
  const shown = all ? routes : routes.slice(0, ROUTES_FOLDED)
  const open = (name: string) => void navigate({ to: '/network/$name', params: { name } })
  const title = t('network.routes.tableTitle')
  return (
    <section aria-label={title}>
      <Section title={title} description={exit ? t('network.routes.tableAside', { count: routes.length, exit: exit.interface }) : t('network.routes.tableAsideNone', { count: routes.length })} flush>
        <DataTable<RouteRow>
          rows={shown}
          rowKey={(route) => `${route.interface}-${route.destination}-${route.via ?? ''}-${route.metric ?? ''}`}
          empty={t('network.routes.noRoutes')}
          columns={[
            {
              id: 'destination',
              header: t('network.routes.columns.destination'),
              cell: (route) => route.prefix === 0
                ? <span className="flex items-center gap-2">{t('network.routes.default')}{route.role ? <Badge variant={route.role === 'effective' ? 'default' : 'outline'}>{t(`network.routes.roles.${route.role}`)}</Badge> : null}</span>
                : <span className="font-mono">{route.destination}</span>,
            },
            { id: 'via', header: t('network.routes.columns.via'), cell: (route) => <span className="font-mono">{route.via ?? t('network.values.onLink')}</span> },
            { id: 'interface', header: t('network.routes.columns.interface'), cell: (route) => <button type="button" className="font-mono text-primary hover:underline" onClick={() => open(route.interface)}>{route.interface}</button> },
            { id: 'metric', header: t('network.routes.columns.metric'), cell: (route) => route.metric ?? '—' },
            { id: 'source', header: t('network.routes.columns.source'), cell: (route) => t(`network.routes.sources.${route.source}`) },
          ]}
        />
        {routes.length > ROUTES_FOLDED && !all ? (
          <div className="border-t px-4 py-2"><Button size="xs" variant="link" onClick={() => setAll(true)}>{t('network.routes.showAll', { count: routes.length })}</Button></div>
        ) : null}
      </Section>
    </section>
  )
}

/// Every interface's static routes in one list. Each change is staged on the
/// interface the route names and applied with the others from the change bar.
function StaticRoutesCard({ data }: { data: NetworkData }) {
  const { t } = useTranslation()
  const [form, setForm] = useState<{ editing?: StaticRow }>()
  const view = data.staged
  const rows = staticRoutes(view, data.set?.base ?? {})
  const title = t('network.fields.routes')
  return (
    <section aria-label={title}>
      <Section
        title={title}
        description={t('network.routes.staticCopy')}
        action={data.set ? <Button size="sm" variant="outline" onClick={() => setForm({})}><Plus />{t('network.routes.add')}</Button> : null}
        flush
      >
        <DataTable<StaticRow>
          rows={rows}
          rowKey={(row) => `${row.iface}-${row.index}`}
          empty={t('network.routes.noStatic')}
          columns={[
            { id: 'destination', header: t('network.routes.columns.destination'), cell: (row) => <span className="font-mono">{row.route.destination}</span> },
            { id: 'gateway', header: t('network.fields.gateway'), cell: (row) => <span className="font-mono">{row.route.gateway ?? t('network.values.onLink')}</span> },
            { id: 'interface', header: t('network.routes.columns.interface'), cell: (row) => <span className="font-mono">{row.iface}</span> },
            { id: 'metric', header: t('network.routes.columns.metric'), cell: (row) => row.route.metric ?? 0 },
            {
              id: 'actions',
              header: '',
              align: 'end',
              cell: (row) => (
                <span className="flex items-center justify-end gap-1">
                  {row.pending ? <Tag tone="pending">{t('network.tags.pending')}</Tag> : null}
                  <Button size="icon-sm" variant="ghost" aria-label={t('network.routes.editNamed', { destination: row.route.destination })} onClick={() => setForm({ editing: row })}><Pencil /></Button>
                  <Button size="icon-sm" variant="ghost" aria-label={t('network.routes.deleteNamed', { destination: row.route.destination })} onClick={() => updateChangeSet((set) => unstageRoute(set, view, row))}><X /></Button>
                </span>
              ),
            },
          ]}
        />
      </Section>
      {form ? <RouteDialog data={data} editing={form.editing} onClose={() => setForm(undefined)} /> : null}
    </section>
  )
}

function RouteDialog({ data, editing, onClose }: { data: NetworkData; editing?: StaticRow; onClose: () => void }) {
  const { t } = useTranslation()
  const view = data.staged
  const targets = routeTargets(view, data.openrc)
  const exit = exitOf(data.applied)
  const [draft, setDraft] = useState<RouteDraft>(() => editing
    ? draftOf(editing)
    : { iface: exit && targets.includes(exit) ? exit : targets[0] ?? '', destination: '', gateway: '', metric: '0' })
  const problems = draftProblems(draft, view, editing)
  const field = (key: keyof RouteDraft) => (value: string) => setDraft({ ...draft, [key]: value })
  const problem = (key: keyof typeof problems) => problems[key] ? <span className="text-destructive">{t(problems[key].key, problems[key].params)}</span> : undefined
  const stage = () => {
    updateChangeSet((set) => stageRoute(set, view, draft, editing))
    onClose()
  }
  const title = t(editing ? 'network.routes.editTitle' : 'network.routes.add')
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose() }}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{t('network.routes.dialogCopy')}</DialogDescription>
        </DialogHeader>
        <DialogBody className="grid gap-4 py-1">
          <FormField label={t('network.routes.columns.destination')} hint={draft.destination ? problem('destination') : t('network.routes.destinationHint')}>
            {(id) => <Input id={id} className="font-mono" value={draft.destination} placeholder="10.20.0.0/16" aria-invalid={Boolean(draft.destination && problems.destination)} onChange={(event) => field('destination')(event.target.value.trim())} />}
          </FormField>
          <FormField label={t('network.fields.gateway')} hint={problem('gateway') ?? t('network.routes.gatewayHint')}>
            {(id) => <Input id={id} className="font-mono" value={draft.gateway} placeholder={t('common.optional')} aria-invalid={Boolean(problems.gateway)} onChange={(event) => field('gateway')(event.target.value.trim())} />}
          </FormField>
          <FormField label={t('network.routes.columns.interface')} hint={problem('iface')}>
            {(id) => (
              <Select value={draft.iface} onValueChange={(value) => field('iface')(String(value))}>
                <SelectTrigger id={id} className="w-48 font-mono"><SelectValue /></SelectTrigger>
                <SelectContent>{targets.map((each) => <SelectItem key={each} value={each}>{each}</SelectItem>)}</SelectContent>
              </Select>
            )}
          </FormField>
          <FormField label={t('network.routes.columns.metric')} hint={problem('metric') ?? t('network.routes.metricHint')}>
            {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={draft.metric} aria-invalid={Boolean(problems.metric)} onChange={(event) => field('metric')(event.target.value.trim())} />}
          </FormField>
        </DialogBody>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t('common.actions.cancel')}</Button>
          <Button disabled={Object.keys(problems).length > 0} onClick={stage}>{t('network.wizard.stage')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
