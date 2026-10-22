import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { FactoryResetPanel } from './factory-reset-panel'

const ready = { supported: true, refusal: null, last: null }

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the factory reset', () => {
  it('says what is erased and kept, asks for the password and reports the reboot', async () => {
    const fetch = stubFetch({
      '/api/v1/system/factory-reset': ready,
      'POST /api/v1/system/factory-reset': () => jsonResponse({ applies: 'reboot' }, 202),
    })
    renderPanel(<FactoryResetPanel />)

    expect(screen.getByText(/software stays at its current version; everything you configured is erased/)).toBeTruthy()
    expect(screen.getByText(/the device is unclaimed afterwards/)).toBeTruthy()
    expect(screen.getByText(/The device code on the label/)).toBeTruthy()
    expect(screen.getByText(/Not a certified wipe/)).toBeTruthy()

    await userEvent.click(await screen.findByRole('button', { name: 'Factory reset…' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'hunter2secret')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Erase and reboot' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'POST')).toBe(true))
    const post = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'POST')!
    expect((post[1] as RequestInit).body).toBe('{"password":"hunter2secret"}')
    expect(await screen.findByText('The device is rebooting and erasing.')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Factory reset…' })).toHaveProperty('disabled', true)
  })

  it('keeps the dialog open on a wrong password and says so', async () => {
    stubFetch({
      '/api/v1/system/factory-reset': ready,
      'POST /api/v1/system/factory-reset': () => jsonResponse({ error: { code: 'password_incorrect', message: 'the password is not the administrator\'s', source: 'micad' } }, 403),
    })
    renderPanel(<FactoryResetPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Factory reset…' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'a guess')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Erase and reboot' }))

    expect(await screen.findByText(/the password is not the administrator's/)).toBeTruthy()
    expect(screen.getByLabelText('Administrator password')).toBeTruthy()
    expect(screen.queryByText('The device is rebooting and erasing.')).toBeNull()
  })

  it('says why it cannot run now and offers no control', async () => {
    stubFetch({
      '/api/v1/system/factory-reset': { supported: true, refusal: { code: 'update_in_progress', message: 'Deployment e is installed and awaits reboot' }, last: null },
    })
    renderPanel(<FactoryResetPanel />)

    expect(await screen.findByText(/An update is being installed or waits for its reboot/)).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Factory reset…' })).toHaveProperty('disabled', true)
  })

  it('shows the last completed reset', async () => {
    stubFetch({
      '/api/v1/system/factory-reset': { ...ready, last: { requested: 1_756_000_000, completed: 1_756_000_060, by: 'cli', source: 'uid 0', unremoved: 0 } },
    })
    renderPanel(<FactoryResetPanel />)

    expect(await screen.findByText(/Last factory reset completed .* UTC, requested from the device's command line/)).toBeTruthy()
  })
})
