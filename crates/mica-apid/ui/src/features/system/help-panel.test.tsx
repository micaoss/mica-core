import { cleanup, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderRoute, stubFetch } from '@/shared/testing/panel'
import { HelpPanel } from './help-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the help tab', () => {
  it('says no online documentation is configured and links nowhere outside the device', async () => {
    stubFetch({})
    const { container } = renderRoute(<HelpPanel />)

    expect(await screen.findByText('Online documentation is not configured for this version.')).toBeTruthy()
    for (const link of container.querySelectorAll('a')) {
      expect(link.getAttribute('href')).toMatch(/^\//)
    }
  })

  it('offers the local guides, each linking to where the fix is made', async () => {
    stubFetch({})
    renderRoute(<HelpPanel />)

    const guides: [string, string][] = [
      ['An update check failed', '/system#update'],
      ['An offline import failed', '/system#update'],
      ['The clock is not synchronized', '/system#time'],
      ['WireGuard has a handshake but no traffic', '/network?tab=wireguard'],
    ]
    for (const [title, href] of guides) {
      const guide = (await screen.findByRole('heading', { name: title })).closest('section') as HTMLElement
      expect(guide.querySelector('ol')?.children.length).toBeGreaterThan(1)
      expect(guide.querySelector('a')?.getAttribute('href')).toBe(href)
    }
    expect(screen.getByRole('button', { name: 'Copy device information' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Download device information' })).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/design scope|approved design|CG-S|campaign/i)
  })
})
