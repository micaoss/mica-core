import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ThemeProvider } from '@/theme/theme'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { LoginView, SetupView } from './auth'

// The sign-in screen carries the appearance control, so it needs the provider
// behind it even though nothing under test reads the theme.
const withTheme = (node: React.ReactNode) => <ThemeProvider>{node}</ThemeProvider>

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn().mockImplementation(() => ({
    matches: false, media: '', addEventListener: vi.fn(), removeEventListener: vi.fn(),
  })))
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('sign-in', () => {
  it('keeps a refused password on the form rather than in a toast', async () => {
    // The operator is about to retype the field, so the message belongs next
    // to it and has to survive until they do.
    stubFetch({ 'POST /api/v1/session': () => jsonResponse({ error: { code: 'invalid_credentials', message: 'The password is not correct.' } }, 401) })
    renderPanel(withTheme(<LoginView />))

    await userEvent.type(screen.getByLabelText('Admin password'), 'wrong')
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))

    expect(await screen.findByText('The password is not correct.')).toBeTruthy()
  })
})

describe('first-run setup', () => {
  it('refuses to submit until the two passwords agree', async () => {
    const fetch = stubFetch({})
    renderPanel(withTheme(<SetupView />))

    await userEvent.type(screen.getByLabelText('Admin password'), 'correct-horse')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'battery-staple')

    expect(await screen.findByText('Passwords do not match.')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Configure device' }))
    expect(fetch).not.toHaveBeenCalled()
  })

  /// Setup creates one credential: the password the operator just chose. It
  /// mints no API token, so there is no secret to stop and copy, and the
  /// console enters on the session the device answered with.
  it('enters the console on the session setup answered with', async () => {
    const fetch = stubFetch({ 'POST /api/v1/setup': { csrfToken: 'csrf-token' } })
    renderPanel(withTheme(<SetupView />))

    await userEvent.type(screen.getByLabelText('Admin password'), 'correct-horse')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'correct-horse')
    await userEvent.click(screen.getByRole('button', { name: 'Configure device' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/setup')).toBe(true))
    expect(screen.queryByText(/token/i)).toBeNull()
  })

  /// Setup is the device code, the administrator password and an optional
  /// hostname: no access point key, whichever way the device was reached.
  it('asks for the device code, the password and an optional hostname only', async () => {
    const fetch = stubFetch({ 'POST /api/v1/setup': { csrfToken: 'csrf-token' } })
    renderPanel(withTheme(<SetupView />))

    expect(screen.queryByLabelText(/access point key/i)).toBeNull()
    expect(screen.getByText(/A wrong code only delays the next try\./)).toBeTruthy()
    await userEvent.type(screen.getByLabelText('Device code'), 'DFN4-L34C-T8N')
    await userEvent.type(screen.getByLabelText('Hostname'), 'deck-7')
    await userEvent.type(screen.getByLabelText('Admin password'), 'correct-horse')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'correct-horse')
    await userEvent.click(screen.getByRole('button', { name: 'Configure device' }))

    await waitFor(() => expect(fetch).toHaveBeenCalled())
    const body = JSON.parse(String(fetch.mock.calls[0][1]?.body)) as Record<string, unknown>
    expect(body).toEqual({ deviceCode: 'DFN4-L34C-T8N', password: 'correct-horse', hostname: 'deck-7' })
  })

  /// A product that turns the device-code gate off claims without it: the
  /// form has no code field and sends none.
  it('asks for no device code when the product turns the gate off', async () => {
    const fetch = stubFetch({ 'POST /api/v1/setup': { csrfToken: 'csrf-token' } })
    renderPanel(withTheme(<SetupView deviceCode="off" />))

    expect(screen.queryByLabelText('Device code')).toBeNull()
    expect(screen.getByText(/^Set the administrator password/)).toBeTruthy()
    await userEvent.type(screen.getByLabelText('Admin password'), 'correct-horse')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'correct-horse')
    await userEvent.click(screen.getByRole('button', { name: 'Configure device' }))

    await waitFor(() => expect(fetch).toHaveBeenCalled())
    expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toEqual({ password: 'correct-horse' })
  })
})
