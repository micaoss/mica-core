import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { configured, networkRoutes } from '@/shared/testing/network'
import { resetChangeSet } from './change-set'
import { InterfaceDetailPage } from './interface-detail'

afterEach(() => {
  cleanup()
  resetChangeSet()
  sessionStorage.clear()
  vi.unstubAllGlobals()
})

const open = (name: string) => renderParamRoute(<InterfaceDetailPage />, '/network_/$name', `/network_/${name}`)

async function review() {
  const bar = await screen.findByRole('region', { name: 'Pending network changes' })
  await userEvent.click(within(bar).getByRole('button', { name: 'Review and apply' }))
  return screen.findByRole('dialog', { name: 'Review network changes' })
}

describe('one interface', () => {
  it('shows what the device observes on the link', async () => {
    stubFetch(networkRoutes())
    open('eth0')
    expect(await screen.findByRole('heading', { name: 'eth0' })).toBeTruthy()
    expect(screen.getByText('192.168.1.24/24')).toBeTruthy()
    expect(screen.getByText('DHCPv4')).toBeTruthy()
    expect(screen.getByText('02:00:00:00:00:01')).toBeTruthy()
    expect(screen.getAllByText('Exit').length).toBeGreaterThan(0)
  })

  it('says so when the device has no such interface', async () => {
    stubFetch(networkRoutes())
    open('nope0')
    expect(await screen.findByText('There is no interface nope0 on this device.')).toBeTruthy()
  })

  it('marks an interface nobody declared and offers no restore for it', async () => {
    stubFetch(networkRoutes())
    open('eth2')
    expect(await screen.findByText('Image default')).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Restore image default' })).toBeNull()
  })

  it('stages a DHCP pool typed as a range, stored as an offset and a size', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    const start = screen.getByLabelText('Pool start')
    const end = screen.getByLabelText('Pool end')
    expect((start as HTMLInputElement).value).toBe('192.168.10.100')
    await userEvent.clear(start)
    await userEvent.type(start, '192.168.10.50')
    await userEvent.clear(end)
    await userEvent.type(end, '192.168.10.59')
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth1' })).getByRole('button', { name: 'Done' }))

    const dialog = await review()
    expect(within(dialog).getByText('192.168.10.100 – 192.168.10.149')).toBeTruthy()
    expect(within(dialog).getByText('192.168.10.50 – 192.168.10.59')).toBeTruthy()
  })

  it('refuses a gateway outside the subnet before anything is staged', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    await userEvent.type(screen.getByLabelText('Gateway'), '10.0.0.1')
    expect(await screen.findByText('The gateway is outside this subnet.')).toBeTruthy()
    const bar = screen.getByRole('region', { name: 'Editing eth1' })
    expect(within(bar).getByText('1 problems to fix')).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Done' }).hasAttribute('disabled')).toBe(true)
  })

  it('puts everything back when an edit is cancelled', async () => {
    stubFetch(networkRoutes())
    open('eth0')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Static' }))
    expect((screen.getByLabelText('Address') as HTMLInputElement).value).toBe('192.168.20.1/24')
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth0' })).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Editing eth0' })).toBeNull())
    expect(screen.queryByRole('region', { name: 'Pending network changes' })).toBeNull()
  })

  it('stages an Ethernet port back to the image default', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Restore image default' }))
    const dialog = await review()
    expect(within(dialog).getByText('eth1')).toBeTruthy()
    expect(within(dialog).getByText('Changed')).toBeTruthy()
  })

  it('will not delete a link that carries a VLAN, and lets a bridge port leave its bridge', async () => {
    const map = { ...configured(), br0: { kind: 'bridge', dhcp: true, bridge: { ports: ['eth2'] } }, 'eth1-20': { kind: 'vlan', dhcp: true, vlan: { parent: 'eth1', id: 20 } } }
    stubFetch(networkRoutes(map))
    open('eth1')
    expect(await screen.findByText('Not possible while VLAN eth1-20 sit on it.')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Restore image default' }).hasAttribute('disabled')).toBe(true)
    cleanup()

    open('eth2')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit' }))
    await userEvent.click(screen.getByRole('button', { name: 'Remove from br0' }))
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth2' })).getByRole('button', { name: 'Done' }))
    const dialog = await review()
    expect(within(dialog).getByText('br0')).toBeTruthy()
  })

  it('sends a radio and a tunnel to their own tabs', async () => {
    stubFetch(networkRoutes())
    open('wlan0')
    expect(await screen.findByRole('button', { name: 'This radio is configured on the Wi-Fi tab.' })).toBeTruthy()
  })
})
