import { describe, expect, it } from 'vitest'
import { en } from './resources'

/// Every `t('a.b.c')` in the tree resolves to a string in the catalogue.

/// Every non-test source in the tree, read eagerly at transform time so the
/// check needs no filesystem access of its own.
const sources = import.meta.glob('/src/**/*.{ts,tsx}', { query: '?raw', import: 'default', eager: true }) as Record<string, string>

function resolve(catalogue: unknown, path: string): unknown {
  return path.split('.').reduce<unknown>((node, segment) => {
    if (node && typeof node === 'object' && segment in node) {
      return (node as Record<string, unknown>)[segment]
    }
    return undefined
  }, catalogue)
}

describe('the translation catalogue', () => {
  it('answers every literal key the console asks for', () => {
    const missing: string[] = []
    const checked: string[] = []
    for (const [file, source] of Object.entries(sources)) {
      if (/\.test\.tsx?$/.test(file)) continue
      for (const match of source.matchAll(/\bt\(\s*'([a-zA-Z0-9_.-]+)'/g)) {
        const key = match[1]
        if (!key.includes('.')) continue
        checked.push(key)
        if (typeof resolve(en, key) !== 'string') missing.push(`${file}: ${key}`)
      }
    }
    // An empty search space would pass this check forever, so the size of it
    // is asserted too.
    expect(checked.length).toBeGreaterThan(300)
    expect(missing).toEqual([])
  })
})
