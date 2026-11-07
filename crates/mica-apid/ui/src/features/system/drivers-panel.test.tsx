import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { DriversPanel } from './drivers-panel'

const KERNEL = 'a'.repeat(64)
const NEW_KERNEL = 'b'.repeat(64)
const SHA = 'c'.repeat(64)

const task = { id: 't1', operation: 'x', kind: 'drivers.apply', dotPath: 'drivers', source: 'apid', status: 'finished', phase: 'applied', outcome: 'succeeded', enqueuedAt: '' }

const extensions = {
  kernel: KERNEL,
  installed: { [KERNEL]: { wifi: 'w1', can: 'c1', gone: 'g1' } },
  pending: { [KERNEL]: { wifi: 'w1', can: 'c2' } },
  pair: null,
  attemptsLeft: 2,
  pendingVerification: ['c2'],
  waitsForExtension: [{ name: 'wifi', kernel: NEW_KERNEL }],
  boot: {
    extensions: [
      { name: 'wifi', id: 'w1', version: '1.2', state: 'mapped' },
      { name: 'can', id: 'c1', state: 'refused', reason: 'signature does not verify' },
      { name: 'gone', id: 'g1', state: 'unused' },
    ],
  },
}

function routes(extra: Record<string, unknown> = {}) {
  return stubFetch({
    '/api/v1/system/owner-certs': [
      { sha256: SHA, state: 'pending-confirmation', subject: 'CN=Fleet owner', key: 'ECDSA P-256' },
    ],
    '/api/v1/system/extensions': extensions,
    '/api/v1/tasks/t1': task,
    ...extra,
  })
}

function posted(fetch: ReturnType<typeof stubFetch>, method: string, path: string) {
  return fetch.mock.calls.find(([input, init]) => String(input) === path && (init as RequestInit | undefined)?.method === method)?.[1] as RequestInit | undefined
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the drivers panel', () => {
  it('shows each certificate and extension as it stands', async () => {
    routes()
    renderPanel(<DriversPanel />)

    expect(await screen.findByText('CN=Fleet owner')).toBeTruthy()
    expect(screen.getByText('Awaits console confirmation')).toBeTruthy()
    expect(await screen.findByText('wifi 1.2')).toBeTruthy()
    expect(screen.getByText('Loaded')).toBeTruthy()
    // c2 replaces the refused c1 on trial: it is verified at the next boot.
    expect(screen.getByText('Verified at next boot')).toBeTruthy()
    expect(screen.getByText('Removed at next boot')).toBeTruthy()
    expect(screen.getByText('A change is on trial: 2 boots left to confirm it.')).toBeTruthy()
    expect(screen.getByText('The staged update waits for driver extensions')).toBeTruthy()
  })

  it('adds a certificate with the password, in base64', async () => {
    const fetch = routes({ 'POST /api/v1/system/owner-certs': () => jsonResponse({ taskId: 't1' }, 202) })
    renderPanel(<DriversPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Add certificate' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.upload(dialog.querySelector('input[type=file]')!, new File(['PEM'], 'owner.pem'))
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'hunter2secret')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Add' }))

    await waitFor(() => expect(posted(fetch, 'POST', '/api/v1/system/owner-certs')).toBeTruthy())
    expect(posted(fetch, 'POST', '/api/v1/system/owner-certs')!.body).toBe('{"password":"hunter2secret","certificate":"UEVN"}')
    expect(await screen.findByText('Change accepted; it applies from the next boot.')).toBeTruthy()
  })

  it('keeps the dialog open on a wrong password', async () => {
    routes({
      [`DELETE /api/v1/system/owner-certs/${SHA}`]: () => jsonResponse({ error: { code: 'password_incorrect', message: 'the password is not the administrator\'s' } }, 403),
    })
    renderPanel(<DriversPanel />)

    // The certificates come first on the page, so their row's Remove is the first.
    await screen.findByText('CN=Fleet owner')
    await userEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0])
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'a guess')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))

    expect(await screen.findByText(/the password is not the administrator's/)).toBeTruthy()
    expect(screen.getByLabelText('Administrator password')).toBeTruthy()
  })

  it('uploads the three files of a build as a form', async () => {
    const fetch = routes({ 'POST /api/v1/system/extensions': () => jsonResponse({ taskId: 't1' }, 202) })
    renderPanel(<DriversPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Upload extension' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.click(within(dialog).getByRole('tab', { name: 'Three files' }))
    // The hidden inputs in field order: descriptor, image, signature.
    const [descriptor, image, signature] = within(dialog).getByRole('tabpanel').querySelectorAll('input[type=file]')
    await userEvent.upload(descriptor as HTMLInputElement, new File(['{}'], 'ext.json'))
    await userEvent.upload(image as HTMLInputElement, new File(['img'], 'ext.img'))
    await userEvent.upload(signature as HTMLInputElement, new File(['sig'], 'ext.roothash.p7s'))
    expect(within(dialog).getByText('ext.roothash.p7s')).toBeTruthy()
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'hunter2secret')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Upload' }))

    await waitFor(() => expect(posted(fetch, 'POST', '/api/v1/system/extensions')).toBeTruthy())
    const form = posted(fetch, 'POST', '/api/v1/system/extensions')!.body as FormData
    expect([...form.keys()]).toEqual(['password', 'descriptor', 'image', 'signature'])
    expect(new Headers(posted(fetch, 'POST', '/api/v1/system/extensions')!.headers).has('content-type')).toBe(false)
  })

  it('lets the update go without an extension the new kernel lacks', async () => {
    const fetch = routes({ 'POST /api/v1/system/extensions/wifi/drop': () => jsonResponse({ taskId: 't1' }, 202) })
    renderPanel(<DriversPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Update without it' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Administrator password'), 'hunter2secret')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Update without it' }))

    await waitFor(() => expect(posted(fetch, 'POST', '/api/v1/system/extensions/wifi/drop')).toBeTruthy())
    expect(posted(fetch, 'POST', '/api/v1/system/extensions/wifi/drop')!.body).toBe(`{"password":"hunter2secret","kernel":"${NEW_KERNEL}"}`)
  })
})
