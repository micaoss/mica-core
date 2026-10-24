import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { configured, networkRoutes, observed } from '@/shared/testing/network'
import { resetChangeSet, updateChangeSet } from './change-set'
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
    expect(screen.getByText('1 h 0 min')).toBeTruthy()
    expect(screen.getAllByText('Exit').length).toBeGreaterThan(0)
  })

  it('heads each block with its title and keeps the edit entry in that header', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    const config = (await screen.findByRole('heading', { name: 'Configuration' })).closest('header')!
    expect(within(config).getByRole('button', { name: 'Edit configuration' })).toBeTruthy()
    const live = screen.getByRole('heading', { name: 'Live state' }).closest('header')!
    expect(within(live).queryByRole('button')).toBeNull()
    expect(screen.getByRole('heading', { name: 'Restore image default' }).className).toContain('text-destructive')
  })

  it('sends the radio to the Wi-Fi tab and a tunnel to its WireGuard page', async () => {
    stubFetch(networkRoutes())
    const radio = open('wlan0')
    await waitFor(() => expect(radio.router.state.location.href).toBe('/network?tab=wifi'))
    cleanup()
    const tunnel = open('wg0')
    await waitFor(() => expect(tunnel.router.state.location.href).toBe('/network?tab=wireguard&tunnel=wg0'))
  })

  it('heads the page with the way back to its tab and no interface switcher', async () => {
    stubFetch(networkRoutes())
    open('eth0')
    const back = await screen.findByRole('link', { name: 'Network / Interfaces' })
    expect(back.getAttribute('href')).toBe('/network?tab=interfaces')
    expect(screen.queryByRole('combobox')).toBeNull()
  })

  it('takes a port off the internet, tags it and warns when no port is left on it', async () => {
    stubFetch(networkRoutes({ ...configured(), eth1: { dhcp: false, static: { address: '192.168.10.1/24' }, internet: false }, eth2: { dhcp: true, internet: false } }))
    open('eth0')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    expect(screen.getByText('When off, this interface never becomes the default route, even when DHCP offers it a gateway.')).toBeTruthy()
    await userEvent.click(screen.getByRole('switch', { name: /^Internet access/ }))
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth0' })).getByRole('button', { name: 'Done' }))
    expect(await screen.findByText('LAN only')).toBeTruthy()
    const dialog = await review()
    expect(within(dialog).getByText('No interface is used for internet access')).toBeTruthy()
  })

  it('shows the board\'s label and keeps an internal port off the internet until it is turned on', async () => {
    const status = observed()
    status.board = 'declared'
    for (const entry of status.interfaces.entries!) {
      if (entry.name === 'eth0') entry.declared = { kind: 'ethernet', label: 'LAN2', order: 1, role: 'internal', capabilities: {} }
      if (entry.name === 'eth1') entry.declared = { kind: 'ethernet', label: 'LAN1', order: 0, role: 'uplink', capabilities: {} }
    }
    stubFetch(networkRoutes(configured(), status))
    open('eth0')
    expect(await screen.findByText('LAN2 · Ethernet port')).toBeTruthy()
    expect(screen.getByText('LAN only')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    const toggle = screen.getByRole('switch', { name: /^Internet access/ })
    expect(toggle.getAttribute('aria-checked')).toBe('false')
    await userEvent.click(toggle)
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth0' })).getByRole('button', { name: 'Done' }))
    let staged: unknown
    updateChangeSet((set) => { staged = set.staged.eth0; return set })
    expect(staged).toEqual({ dhcp: true, internet: true })
    cleanup()

    open('eth2')
    expect(await screen.findByText('Undeclared')).toBeTruthy()
  })

  it('refuses a static gateway on a port off the internet', async () => {
    stubFetch(networkRoutes({ ...configured(), eth1: { dhcp: false, static: { address: '192.168.10.1/24' }, internet: false } }))
    open('eth1')
    expect(await screen.findByText('LAN only')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    expect(screen.getByRole('switch', { name: /^Internet access/ }).getAttribute('aria-checked')).toBe('false')
    await userEvent.type(screen.getByLabelText('Gateway'), '192.168.10.254')
    expect(await screen.findByText('This interface is not used for internet access, so it cannot have a gateway.')).toBeTruthy()
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

  it('refuses a gateway outside the subnet before anything is staged', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.type(screen.getByLabelText('Gateway'), '10.0.0.1')
    expect(await screen.findByText('The gateway is outside this subnet.')).toBeTruthy()
    const bar = screen.getByRole('region', { name: 'Editing eth1' })
    expect(within(bar).getByText('1 problem to fix')).toBeTruthy()
    expect(within(bar).getByRole('button', { name: 'Done' }).hasAttribute('disabled')).toBe(true)
  })

  it('puts everything back when an edit is cancelled', async () => {
    stubFetch(networkRoutes())
    open('eth0')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Static IP address' }))
    expect((screen.getByLabelText('Address') as HTMLInputElement).value).toBe('')
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth0' })).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Editing eth0' })).toBeNull())
    expect(screen.queryByRole('region', { name: 'Pending network changes' })).toBeNull()
  })

  it('prefills nothing when a port goes static, and offers the lease it holds', async () => {
    stubFetch(networkRoutes())
    open('eth0')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Static IP address' }))
    expect((screen.getByLabelText('Address') as HTMLInputElement).value).toBe('')
    expect((screen.getByLabelText('Gateway') as HTMLInputElement).value).toBe('')
    await userEvent.click(screen.getByRole('button', { name: 'Use the current lease (192.168.1.24/24, gateway 192.168.1.1)' }))
    expect((screen.getByLabelText('Address') as HTMLInputElement).value).toBe('192.168.1.24/24')
    expect((screen.getByLabelText('Gateway') as HTMLInputElement).value).toBe('192.168.1.1')
    expect((screen.getByLabelText('DNS servers') as HTMLInputElement).value).toBe('192.168.1.1')
    expect(screen.queryByRole('button', { name: /Use the current lease/ })).toBeNull()
  })

  it('offers recommended DNS servers to a static port that has none', async () => {
    stubFetch(networkRoutes())
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Recommended DNS: 8.8.8.8, 1.1.1.1' }))
    expect((screen.getByLabelText('DNS servers') as HTMLInputElement).value).toBe('8.8.8.8, 1.1.1.1')
    expect(screen.queryByRole('button', { name: /Recommended DNS/ })).toBeNull()
  })

  it('offers no lease on a port that holds none', async () => {
    stubFetch(networkRoutes({ ...configured(), eth1: { dhcp: true } }))
    open('eth1')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Static IP address' }))
    expect((screen.getByLabelText('Address') as HTMLInputElement).value).toBe('')
    expect(screen.queryByRole('button', { name: /Use the current lease/ })).toBeNull()
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
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    await userEvent.click(screen.getByRole('button', { name: 'Remove from br0' }))
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing eth2' })).getByRole('button', { name: 'Done' }))
    const dialog = await review()
    expect(within(dialog).getByText('br0')).toBeTruthy()
  })
})

describe('a CAN bus', () => {
  it('names its controller state and bit rate, and what restoring drops', async () => {
    stubFetch(networkRoutes())
    open('can0')
    expect(await screen.findByText('Normal (error-active)')).toBeTruthy()
    expect(screen.getAllByText('250 kbit/s').length).toBeGreaterThan(0)
    expect(screen.queryByText('error-active')).toBeNull()
    expect(screen.getByText('This drops the CAN settings this console made and returns the bus to the image\'s default.')).toBeTruthy()
  })

  it('shows the routes read-only and leads to the routes tab to manage them', async () => {
    stubFetch(networkRoutes({ ...configured(), eth1: { dhcp: false, static: { address: '192.168.10.1/24' }, routes: [{ destination: '10.20.0.0/16', gateway: '192.168.10.2', metric: 5 }] } }))
    const { router } = open('eth1')
    expect(await screen.findByText('10.20.0.0/16 → 192.168.10.2 · metric 5')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Edit configuration' }))
    expect(screen.queryByRole('button', { name: 'Add route' })).toBeNull()
    expect(screen.getByText('10.20.0.0/16 → 192.168.10.2 · metric 5')).toBeTruthy()
    await userEvent.click(screen.getByRole('link', { name: 'Manage on the routes page' }))
    await waitFor(() => expect(router.state.location.href).toBe('/network?tab=routes'))
  })
})
