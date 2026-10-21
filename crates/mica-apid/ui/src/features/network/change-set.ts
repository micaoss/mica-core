import { useEffect, useSyncExternalStore } from 'react'
import { discardAll, rebase, type ChangeSet, type NetworkMap } from './model'

/// The staged change set, shared by the network pages and kept in session
/// storage per device, so a reload or a move between the list and an
/// interface's page keeps it. One write applies it.

export interface Applying {
  taskId: string
  /// The map as it was stored before the write, for "restore".
  previous: NetworkMap
  /// Whether this write is the restore itself.
  restoring: boolean
}

interface State {
  host?: string
  set?: ChangeSet
  /// Entries the set touches that the device changed underneath it.
  conflicts: string[]
  fresh?: NetworkMap
  applying?: Applying
}

let state: State = { conflicts: [] }
const listeners = new Set<() => void>()

const storageKey = (host: string) => `mica.network.changes.${host}`

function emit(next: State) {
  state = next
  if (next.host && next.set) {
    try {
      sessionStorage.setItem(storageKey(next.host), JSON.stringify(next.set))
    } catch {
      // Storage refused (private mode, quota): the set lives for this page only.
    }
  }
  for (const listener of listeners) listener()
}

function stored(host: string): ChangeSet | undefined {
  try {
    const text = sessionStorage.getItem(storageKey(host))
    const value = text ? JSON.parse(text) as ChangeSet : undefined
    return value && typeof value.base === 'object' && typeof value.staged === 'object' ? value : undefined
  } catch {
    return undefined
  }
}

/// Take a fresh read of the device's map for `host`.
export function syncChangeSet(host: string, fresh: NetworkMap) {
  const current = state.host === host ? state.set : stored(host)
  if (!current) {
    emit({ ...state, host, set: { base: fresh, staged: fresh }, conflicts: [], fresh })
    return
  }
  const { set, conflicts } = rebase(current, fresh)
  emit({ ...state, host, set, conflicts, fresh })
}

export function updateChangeSet(change: (set: ChangeSet) => ChangeSet) {
  if (!state.set) return
  emit({ ...state, set: change(state.set) })
}

/// Drop every staged change. After a conflict the set starts again from the
/// device's current map.
export function discardChangeSet() {
  if (!state.set) return
  const set = state.conflicts.length && state.fresh ? { base: state.fresh, staged: state.fresh } : discardAll(state.set)
  emit({ ...state, set, conflicts: [] })
}

/// The device stored `map`: it is the new base, and nothing is staged.
export function settleChangeSet(map: NetworkMap, applying?: Applying) {
  emit({ ...state, set: { base: map, staged: map }, conflicts: [], fresh: map, applying })
}

export function finishApplying() {
  emit({ ...state, applying: undefined })
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

/// The change set for `host`, following `fresh` reads of its map.
export function useChangeSet(host: string | undefined, fresh: NetworkMap | undefined) {
  const current = useSyncExternalStore(subscribe, () => state)
  useEffect(() => {
    if (host && fresh) syncChangeSet(host, fresh)
  }, [host, fresh])
  return current.host === host ? current : { conflicts: [] }
}

/// For tests: forget everything.
export function resetChangeSet() {
  state = { conflicts: [] }
  for (const listener of listeners) listener()
}
