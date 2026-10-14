import { I18nextProvider } from 'react-i18next'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { i18n } from '@/i18n/i18n'
import { LOCALE_STORAGE_KEY } from '@/i18n/locale'
import { ThemeProvider, THEME_STORAGE_KEY } from '@/theme/theme'
import { Preferences } from './preferences'

beforeEach(async () => {
  window.localStorage.clear()
  document.documentElement.className = ''
  document.documentElement.lang = 'en'
  vi.stubGlobal('matchMedia', vi.fn().mockImplementation(() => ({
    matches: false,
    media: '(prefers-color-scheme: dark)',
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })))
  await i18n.changeLanguage('en')
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

function renderPreferences() {
  return render(
    <I18nextProvider i18n={i18n}>
      <ThemeProvider>
        <Preferences />
      </ThemeProvider>
    </I18nextProvider>,
  )
}

async function openPicker() {
  const trigger = screen.getByRole('combobox', { name: 'Language' })
  await userEvent.click(trigger)
  return trigger
}

describe('display preferences', () => {
  it('applies a language and persists the choice', async () => {
    renderPreferences()

    await openPicker()
    await userEvent.click(await screen.findByRole('option', { name: /简体中文/ }))

    await waitFor(() => expect(document.documentElement.lang).toBe('zh-CN'))
    expect(window.localStorage.getItem(LOCALE_STORAGE_KEY)).toBe('zh-CN')
  })

  /// A language the console has no bundle for is still offered, labelled, and
  /// still recorded as the operator's choice. It renders in English rather
  /// than silently reverting to whatever the browser prefers.
  it('marks a language it cannot render and falls back to English', async () => {
    renderPreferences()

    await openPicker()
    const japanese = await screen.findByRole('option', { name: /日本語/ })
    expect(japanese.textContent).toContain('Planned')
    await userEvent.click(japanese)

    await waitFor(() => expect(window.localStorage.getItem(LOCALE_STORAGE_KEY)).toBe('ja'))
    expect(document.documentElement.lang).toBe('en')
  })

  /// Both controls are the same shape, which is the point: one icon trigger
  /// each, the current value checked in the popup.
  it('changes and persists an explicit theme', async () => {
    renderPreferences()

    await userEvent.click(screen.getByRole('combobox', { name: 'Appearance' }))
    await userEvent.click(await screen.findByRole('option', { name: 'Dark' }))

    await waitFor(() => expect(document.documentElement.classList.contains('dark')).toBe(true))
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark')
  })

  /// Neither control is a text box: in a header slot a search input reads as an
  /// empty search field rather than as the language the console is in.
  it('is a pair of triggers rather than a text input', async () => {
    renderPreferences()

    const language = screen.getByRole('combobox', { name: 'Language' })
    const appearance = screen.getByRole('combobox', { name: 'Appearance' })

    expect(language.tagName).not.toBe('INPUT')
    expect(appearance.tagName).not.toBe('INPUT')
    // The value is there for a screen reader and hidden from the layout, so
    // the trigger stays icon-width.
    expect(language.querySelector('.sr-only')).toBeTruthy()
  })

  /// The picker anchors to its own trigger rather than opening a dialog inside
  /// a menu, so there is no second layer to leave open behind it.
  it('opens no dialog layer of its own', async () => {
    renderPreferences()

    await openPicker()

    expect(await screen.findByRole('listbox')).toBeTruthy()
    expect(screen.queryByRole('dialog')).toBeNull()
  })
})
