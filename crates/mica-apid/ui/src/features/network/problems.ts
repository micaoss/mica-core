import { bridgeOf, type Change } from './model'
import { addressBook, validateEntry, type Problems } from './validate'
import type { NetView } from './view'

/// What the console would refuse in each changed interface, keyed by name.
/// An interface with nothing to correct is absent.
export function stagedProblems(staged: NetView, changes: Change[]): Record<string, Problems> {
  const observed = Object.fromEntries(staged.order.map((name) => [name, staged.ifaces[name].obs?.address]))
  const book = addressBook(staged.map, observed)
  const out: Record<string, Problems> = {}
  for (const change of changes) {
    const entry = staged.map[change.name]
    const problems = validateEntry(change.name, change.kind, entry, staged.map, book, bridgeOf(staged.map, change.name) !== undefined)
    if (Object.keys(problems).length > 0) out[change.name] = problems
  }
  return out
}

export function problemCount(problems: Record<string, Problems>): number {
  return Object.values(problems).reduce((sum, each) => sum + Object.keys(each).length, 0)
}
