import { cleanup, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { i18n, setLocaleChoice } from '@/i18n/i18n'
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

  it('links the documentation entry and its pages in the console language, in new tabs without an opener', async () => {
    stubFetch({ '/api/v1/meta': { features: [], init: 'systemd', docsUrl: 'https://micaos.dev/docs/', docsUrlEn: 'https://micaos.dev/en/docs/' } })
    renderRoute(<HelpPanel />)

    const entry = await screen.findByRole('link', { name: /Open the documentation/ })
    expect(entry.getAttribute('href')).toBe('https://micaos.dev/en/docs/')
    expect(screen.getByRole('link', { name: /Troubleshooting \(opens/ }).getAttribute('href')).toBe('https://micaos.dev/en/docs/operate/troubleshooting/')
    expect(screen.getByRole('link', { name: /Recovery/ }).getAttribute('href')).toBe('https://micaos.dev/en/docs/operate/recovery/')
    expect(screen.getByRole('link', { name: /Updates/ }).getAttribute('href')).toBe('https://micaos.dev/en/docs/operate/updates/')
    for (const link of screen.getAllByRole('link', { name: /opens a new tab/ })) {
      expect(link.getAttribute('target')).toBe('_blank')
      expect(link.getAttribute('rel')).toBe('noopener noreferrer')
    }
    expect(screen.queryByText('Online documentation is not configured for this version.')).toBeNull()
  })

  it('links the default tree in Chinese', async () => {
    await setLocaleChoice('zh-CN')
    try {
      stubFetch({ '/api/v1/meta': { features: [], init: 'systemd', docsUrl: 'https://micaos.dev/docs/', docsUrlEn: 'https://micaos.dev/en/docs/' } })
      renderRoute(<HelpPanel />)

      const entry = await screen.findByRole('link', { name: /打开使用文档/ })
      expect(entry.getAttribute('href')).toBe('https://micaos.dev/docs/')
      expect(screen.getByRole('link', { name: /系统恢复/ }).getAttribute('href')).toBe('https://micaos.dev/docs/operate/recovery/')
    } finally {
      window.localStorage.clear()
      await i18n.changeLanguage('en')
    }
  })

  it('falls back to the default tree in English when the product names no English one', async () => {
    stubFetch({ '/api/v1/meta': { features: [], init: 'systemd', docsUrl: 'https://micaos.dev/docs/' } })
    renderRoute(<HelpPanel />)

    const entry = await screen.findByRole('link', { name: /Open the documentation/ })
    expect(entry.getAttribute('href')).toBe('https://micaos.dev/docs/')
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
