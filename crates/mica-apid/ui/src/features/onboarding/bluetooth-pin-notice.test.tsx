import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { renderPanel, stubFetch } from '@/shared/testing/panel'
import { BluetoothPinNotice, PIN_SAVED_KEY } from './bluetooth-pin-notice'

const overview = (pin: string) => ({
  enabled: true,
  discoverable: false,
  pin,
  declared: {},
  adapter: { available: true, address: '11:22:33:44:55:66', alias: 'edge-42', powered: true, discoverable: false, discovering: false },
  devices: { available: true, entries: [] },
  pending: null,
})

beforeEach(() => window.localStorage.clear())
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the Bluetooth PIN notice', () => {
  it('asks the operator to save the code on the first sign-in, until they say they have', async () => {
    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': overview('482913') })
    renderPanel(<BluetoothPinNotice />)

    expect((await screen.findAllByText('482913')).length).toBeGreaterThan(0)
    await userEvent.click(screen.getByRole('button', { name: 'I have saved it' }))
    await waitFor(() => expect(screen.queryAllByText('482913')).toHaveLength(0))
    expect(window.localStorage.getItem(PIN_SAVED_KEY)).toBe('482913')
  })

  it('leaves for now on later, without counting the code as saved', async () => {
    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': overview('482913') })
    renderPanel(<BluetoothPinNotice />)
    expect((await screen.findAllByText('482913')).length).toBeGreaterThan(0)
    await userEvent.click(screen.getByRole('button', { name: 'Remind me later' }))
    await waitFor(() => expect(screen.queryAllByText('482913')).toHaveLength(0))
    expect(window.localStorage.getItem(PIN_SAVED_KEY)).toBeNull()
  })

  it('stays away once this code was saved, and returns for a new one', async () => {
    window.localStorage.setItem(PIN_SAVED_KEY, '482913')
    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': overview('482913') })
    const { unmount } = renderPanel(<BluetoothPinNotice />)
    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(screen.queryAllByText('482913')).toHaveLength(0)
    unmount()

    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': overview('105577') })
    renderPanel(<BluetoothPinNotice />)
    expect((await screen.findAllByText('105577')).length).toBeGreaterThan(0)
  })

  it('says nothing while Bluetooth is switched off', async () => {
    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': { ...overview('482913'), enabled: false } })
    renderPanel(<BluetoothPinNotice />)
    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(screen.queryAllByText('482913')).toHaveLength(0)
  })

  it('does not ask to save the default code', async () => {
    stubFetch({ '/api/v1/meta': { features: ['bluetooth'] }, '/api/v1/bluetooth': overview('000000') })
    renderPanel(<BluetoothPinNotice />)
    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(screen.queryAllByText('000000')).toHaveLength(0)
  })

  it('says nothing on a product without Bluetooth', async () => {
    const fetch = stubFetch({ '/api/v1/meta': { features: ['wifi'] }, '/api/v1/bluetooth': overview('482913') })
    renderPanel(<BluetoothPinNotice />)
    await waitFor(() => expect(fetch.mock.calls.some(([path]) => path === '/api/v1/meta')).toBe(true))
    await new Promise((resolve) => setTimeout(resolve, 50))
    expect(screen.queryAllByText('482913')).toHaveLength(0)
    expect(fetch.mock.calls.some(([path]) => path === '/api/v1/bluetooth')).toBe(false)
  })
})
