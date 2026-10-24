import { bridgeOf, isIPv4, parseCidr, stageSet, type ChangeSet, type NetworkMap, type StaticRoute } from './model'
import type { Problem } from './validate'
import type { NetView } from './view'

/// The static routes of every interface, as the routes tab edits them. Each
/// lives in its interface's `routes`, so a change is staged on that entry.

/// One route, where it lives, and whether the device stores it yet.
export interface StaticRow { iface: string; index: number; route: StaticRoute; pending: boolean }

/// The route form as typed.
export interface RouteDraft { iface: string; destination: string; gateway: string; metric: string }
export type RouteProblems = Partial<Record<'destination' | 'gateway' | 'iface' | 'metric', Problem>>

const sameRoute = (left: StaticRoute, right: StaticRoute) =>
  left.destination === right.destination && (left.gateway ?? '') === (right.gateway ?? '') && (left.metric ?? 0) === (right.metric ?? 0)

export function staticRoutes(view: NetView, base: NetworkMap): StaticRow[] {
  return view.order.flatMap((iface) => (view.map[iface]?.routes ?? []).map((route, index) => ({
    iface,
    index,
    route,
    pending: !(base[iface]?.routes ?? []).some((stored) => sameRoute(stored, route)),
  })))
}

/// The interfaces a route can name: those with addressing of their own. On
/// OpenRC only a physical link's stanza carries routes.
export function routeTargets(view: NetView, openrc: boolean): string[] {
  const kinds = openrc ? ['physical', 'wifi'] : ['physical', 'wifi', 'bridge', 'vlan', 'wireguard']
  return view.order.filter((name) => kinds.includes(view.ifaces[name].kind) && !bridgeOf(view.map, name))
}

export function draftOf(row: StaticRow): RouteDraft {
  return { iface: row.iface, destination: row.route.destination, gateway: row.route.gateway ?? '', metric: String(row.route.metric ?? 0) }
}

function routeOf(draft: RouteDraft): StaticRoute {
  const route: StaticRoute = { destination: draft.destination.trim() }
  if (draft.gateway.trim()) route.gateway = draft.gateway.trim()
  if (Number(draft.metric) > 0) route.metric = Number(draft.metric)
  return route
}

/// What the form refuses: the destination and gateway as an interface's own
/// routes are checked, the default route, which follows each interface's
/// gateway, and a second route to one destination on one interface.
/// `editing` is the row the form replaces.
export function draftProblems(draft: RouteDraft, view: NetView, editing?: StaticRow): RouteProblems {
  const problems: RouteProblems = {}
  const destination = draft.destination.trim()
  if (!parseCidr(destination)) problems.destination = { key: 'network.problems.cidr' }
  else if (parseCidr(destination)!.prefix === 0) problems.destination = { key: 'network.routes.problems.noDefault' }
  else if ((view.map[draft.iface]?.routes ?? []).some((route, index) => route.destination === destination && !(editing?.iface === draft.iface && editing.index === index))) {
    problems.destination = { key: 'network.routes.problems.duplicate', params: { iface: draft.iface, destination } }
  }
  if (draft.gateway.trim() && !isIPv4(draft.gateway.trim())) problems.gateway = { key: 'network.problems.ipv4' }
  if (!draft.iface) problems.iface = { key: 'network.routes.problems.iface' }
  if (!/^\d{1,10}$/.test(draft.metric.trim()) || Number(draft.metric) > 4_294_967_295) problems.metric = { key: 'network.routes.problems.metric' }
  return problems
}

/// Stage `draft`, in place of `editing` when it is set.
export function stageRoute(set: ChangeSet, view: NetView, draft: RouteDraft, editing?: StaticRow): ChangeSet {
  const route = routeOf(draft)
  if (editing && editing.iface === draft.iface) {
    return withRoutes(set, view, draft.iface, (routes) => routes.map((each, index) => index === editing.index ? route : each))
  }
  const next = editing ? unstageRoute(set, view, editing) : set
  return withRoutes(next, view, draft.iface, (routes) => [...routes, route])
}

export function unstageRoute(set: ChangeSet, view: NetView, row: StaticRow): ChangeSet {
  return withRoutes(set, view, row.iface, (routes) => routes.filter((_, index) => index !== row.index))
}

function withRoutes(set: ChangeSet, view: NetView, iface: string, change: (routes: StaticRoute[]) => StaticRoute[]): ChangeSet {
  return stageSet(set, iface, view.ifaces[iface]?.kind ?? 'physical', (entry) => {
    const routes = change(entry.routes ?? [])
    if (routes.length) entry.routes = routes
    else delete entry.routes
    return entry
  })
}
