import { act, cleanup, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { RouterProvider, createMemoryHistory, createRouter } from '@tanstack/react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { AppProviders } from '@/app/providers'
import { queryClient } from '@/app/query-client'
import { routeTree } from '@/app/routeTree.gen'
import { stubFetch } from '@/shared/testing/panel'

/// What apid's policy (`style-src 'self'; script-src 'self'`) refuses in the
/// console: a `<style>` element, a `<script>` without a source, and a
/// stylesheet from anywhere else. jsdom enforces no policy, so every node the
/// page adds is held to it here. A style set through React's `style` prop is
/// CSSOM, which the policy allows.
function refused(node: Node): string | undefined {
  if (!(node instanceof Element)) return undefined
  if (node instanceof HTMLStyleElement) return `<style>${node.textContent ?? ''}</style>`
  if (node instanceof HTMLScriptElement && !node.src) return `<script>${node.textContent ?? ''}</script>`
  if (node instanceof HTMLLinkElement && node.rel === 'stylesheet' && new URL(node.href, location.href).origin !== location.origin) {
    return `<link rel="stylesheet" href="${node.href}">`
  }
  return undefined
}

function watch() {
  const found: string[] = []
  const observer = new MutationObserver((records) => {
    for (const record of records) {
      for (const node of record.addedNodes) {
        for (const each of [node, ...(node instanceof Element ? node.querySelectorAll('*') : [])]) {
          const violation = refused(each)
          if (violation) found.push(violation)
        }
      }
    }
  })
  observer.observe(document, { childList: true, subtree: true })
  return {
    stop: () => {
      for (const record of observer.takeRecords()) {
        for (const node of record.addedNodes) {
          const violation = refused(node)
          if (violation) found.push(violation)
        }
      }
      observer.disconnect()
      for (const node of document.querySelectorAll('*')) {
        const violation = refused(node)
        if (violation && !found.includes(violation)) found.push(violation)
      }
      return found
    },
  }
}

const network = {
  configured: {
    eth0: { kind: 'physical', dhcp: true },
    can0: { kind: 'can', dhcp: false, can: { bitrate: 250000, restartMs: 100, fd: false } },
  },
  configuredCount: 2,
  observed: {
    available: true,
    interfaceCount: 3,
    interfaces: [
      { index: 2, name: 'eth0', type: 'ether' },
      { index: 3, name: 'can0', type: 'can' },
      { index: 4, name: 'can1', type: 'can' },
    ],
  },
}

const routes = {
  '/api/v1/session': { state: 'authenticated', csrfToken: 'csrf' },
  '/api/v1/health': { apid: 'ok', micad: 'ok', checkedAt: 1 },
  '/api/v1/settings/hostname': 'mica-test',
  '/api/v1/claim': { state: 'claimed', rotationRequired: false },
  '/api/v1/network': network,
  '/api/v1/units': {
    init: 'openrc',
    available: true,
    units: [{ name: 'my-service', declared: true, managed: true, installed: true, enabled: true, active: 'started', kind: 'openrc' }],
  },
  '/api/v1/settings/services.units': { 'my-service': { kind: 'openrc', text: '#!/sbin/openrc-run\n', conf: 'X=1\n', enabled: true } },
  '/api/v1/units/my-service/logs': { source: 'my-service', available: true, lines: ['started'], truncated: false },
}

/// Every page of the generated route tree, the undeclared CAN controller's
/// included.
const PAGES = ['/', '/network', '/network/eth0', '/network/can0', '/network/can1', '/services', '/services/units/my-service', '/access', '/system', '/system/ui']

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn().mockImplementation(() => ({
    matches: false, media: '', addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })))
})

afterEach(() => {
  cleanup()
  queryClient.clear()
  vi.unstubAllGlobals()
  document.head.querySelectorAll('style').forEach((node) => node.remove())
})

/// Open what the page offers: each dialog an `Add` button raises, and each
/// select on the page and in that dialog, so a popup's own nodes are added.
async function openEverything() {
  const user = userEvent.setup()
  const selects = async (scope: HTMLElement) => {
    for (const combobox of within(scope).queryAllByRole('combobox')) {
      await user.click(combobox)
      await user.keyboard('{Escape}')
    }
  }
  await selects(document.body)
  for (const add of screen.queryAllByRole('button', { name: /^Add/ })) {
    await user.click(add)
    const dialog = screen.queryByRole('dialog')
    if (dialog) {
      await selects(dialog)
      await user.keyboard('{Escape}')
    }
  }
}

describe('the console under apid\'s content security policy', () => {
  it.each(PAGES)('%s adds nothing the policy refuses', async (page) => {
    stubFetch(routes)
    const watching = watch()
    const router = createRouter({
      routeTree,
      basepath: '/_ui',
      history: createMemoryHistory({ initialEntries: [`/_ui${page}`] }),
    })
    render(<AppProviders><RouterProvider router={router} /></AppProviders>)
    await screen.findByRole('navigation', { name: 'Primary navigation' })
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 50)) })
    await openEverything()

    expect(watching.stop()).toEqual([])
  })
})
