import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderParamRoute, stubFetch } from '@/shared/testing/panel'
import { configured, networkRoutes, observed, PEER_KEY } from '@/shared/testing/network'
import { resetChangeSet } from './change-set'
import { NetworkPage } from './network-page'
import { hostRoute } from './wireguard-tab'

const NEW_PEER = 'bmV3UGVlcktleW5ld1BlZXJLZXluZXdQZWVyS2V5MDA='

afterEach(() => {
  cleanup()
  resetChangeSet()
  sessionStorage.clear()
  vi.unstubAllGlobals()
})

const open = (search: string) => renderParamRoute(<NetworkPage />, '/network', `/network?${search}`)

describe('the WireGuard tab', () => {
  it('lists each tunnel with its port and peers, and opens one', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard')
    const card = await screen.findByRole('link', { name: /wg0/ })
    expect(within(card).getByText('51820')).toBeTruthy()
    expect(within(card).getByText('1 peer')).toBeTruthy()
  })

  it('reads the latest handshake on the card and each peer\'s handshake and traffic in the table', async () => {
    const status = observed()
    const handshake = Math.floor(Date.now() / 1000) - 300
    for (const entry of status.interfaces.entries!) {
      if (entry.name === 'wg0') entry.wireguard = { available: true, peers: [{ publicKey: PEER_KEY, latestHandshake: handshake, rxBytes: 1_310_720, txBytes: 2048, persistentKeepalive: 25 }] }
    }
    stubFetch({
      ...networkRoutes(configured(), status),
      '/api/v1/network/wg0/peers': [{ name: 'Harbour office', publicKey: PEER_KEY, presharedKey: true, allowedIps: ['10.8.0.2/32'], persistentKeepalive: 25, latestHandshake: handshake, rxBytes: 1_310_720, txBytes: 2048 }],
    })
    open('tab=wireguard')
    const card = await screen.findByRole('link', { name: /wg0/ })
    expect(within(card).getByText('Peers offline')).toBeTruthy()
    expect(within(card).getByText('5 min ago')).toBeTruthy()

    await userEvent.click(card)
    const table = await screen.findByRole('table')
    expect(within(table).getByRole('columnheader', { name: 'Latest handshake' })).toBeTruthy()
    expect(within(table).getByText('5 min ago')).toBeTruthy()
    expect(within(table).getByText('1.3 MiB / 2.0 KiB')).toBeTruthy()
  })

  it('says a peer has not handshaken yet', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard&tunnel=wg0')
    const table = await screen.findByRole('table')
    expect(await within(table).findByText('No handshake yet')).toBeTruthy()
  })

  it('gives the remote side this device as a host route on its own key', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard&tunnel=wg0')
    const snippet = await screen.findByText(/\[Peer\]/)
    expect(snippet.textContent).toContain('PublicKey = Q2hhbmdlTWVQbGVhc2VDaGFuZ2VNZVBsZWFzZTAwMDA=')
    expect(snippet.textContent).toContain('AllowedIPs = 10.8.0.1/32')
    expect(snippet.textContent).toContain(':51820')
    expect(hostRoute('fd00::1/64')).toBe('fd00::1/128')
  })

  it('adds a named peer with a pre-shared key straight to the device', async () => {
    const fetch = stubFetch({ ...networkRoutes(), 'POST /api/v1/network/wg0/peers': () => jsonResponse({ publicKey: NEW_PEER, allowedIps: ['10.8.0.3/32'] }) })
    open('tab=wireguard&tunnel=wg0')
    await screen.findByText(`${PEER_KEY.slice(0, 8)}…${PEER_KEY.slice(-6)}`)
    await userEvent.click(screen.getByRole('button', { name: 'Add peer' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Name'), 'Skipper laptop')
    await userEvent.type(within(dialog).getByLabelText('Public key'), NEW_PEER)
    await userEvent.type(within(dialog).getByLabelText('Pre-shared key'), 'short')
    expect(within(dialog).getByText('A WireGuard key is 44 base64 characters.')).toBeTruthy()
    await userEvent.clear(within(dialog).getByLabelText('Pre-shared key'))
    await userEvent.type(within(dialog).getByLabelText('Pre-shared key'), PEER_KEY)
    await userEvent.type(within(dialog).getByLabelText('Allowed IPs'), '10.8.0.3/32')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Add' }))

    await waitFor(() => {
      const call = fetch.mock.calls.find(([input, init]) => String(input) === '/api/v1/network/wg0/peers' && (init as RequestInit | undefined)?.method === 'POST')
      expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({ name: 'Skipper laptop', publicKey: NEW_PEER, presharedKey: PEER_KEY, allowedIps: ['10.8.0.3/32'] })
    })
    expect((await screen.findAllByText('Peer added.')).length).toBeGreaterThan(0)
  })

  it('refuses a key that is already a peer', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard&tunnel=wg0')
    await userEvent.click(await screen.findByRole('button', { name: 'Add peer' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Public key'), PEER_KEY)
    expect(await within(dialog).findByText('That peer is already added.')).toBeTruthy()
  })

  it('shows the new public key after a rotation', async () => {
    stubFetch({ ...networkRoutes(), 'POST /api/v1/actions/wireguard/wg0/rotate-key': () => jsonResponse({ publicKey: NEW_PEER }) })
    open('tab=wireguard&tunnel=wg0')
    await userEvent.click(await screen.findByRole('button', { name: 'Rotate key' }))
    await userEvent.click(within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Rotate key' }))
    expect(await screen.findByText('The key changed')).toBeTruthy()
    expect(screen.getAllByText(NEW_PEER).length).toBeGreaterThan(0)
  })

  it('stages a tunnel delete behind a confirmation naming its peers and subnets, and undoes it', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard&tunnel=wg0')
    await screen.findByText(`${PEER_KEY.slice(0, 8)}…${PEER_KEY.slice(-6)}`)
    await userEvent.click(screen.getByRole('button', { name: 'Delete wg0' }))
    const confirm = await screen.findByRole('alertdialog')
    expect(within(confirm).getByText(/1 peer will disconnect and the routes to 10\.8\.0\.2\/32 will be removed/)).toBeTruthy()
    await userEvent.click(within(confirm).getByRole('button', { name: 'Delete wg0' }))

    const card = await screen.findByRole('link', { name: /wg0/ })
    expect(within(card).getByText('Delete · pending')).toBeTruthy()
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    expect(within(bar).getByText('wg0')).toBeTruthy()

    await userEvent.click(card)
    expect(await screen.findByText('Deleted when the changes are applied.')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Revert' }))
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Pending network changes' })).toBeNull())
    expect(screen.getByRole('button', { name: 'Delete wg0' })).toBeTruthy()
  })

  it('drops a tunnel that was never applied without asking', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard')
    await userEvent.click(await screen.findByRole('button', { name: 'Add interface' }))
    await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Stage' }))
    expect(await screen.findByText('New · pending')).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Undo create' }))
    await waitFor(() => expect(screen.queryByText('New · pending')).toBeNull())
  })

  it('stages a new listen port with the interface changes', async () => {
    stubFetch(networkRoutes())
    open('tab=wireguard&tunnel=wg0')
    await userEvent.click(await screen.findByRole('button', { name: 'Edit configuration' }))
    const port = screen.getByLabelText('Listen port')
    await userEvent.clear(port)
    await userEvent.type(port, '51900')
    await userEvent.click(within(screen.getByRole('region', { name: 'Editing tunnel wg0' })).getByRole('button', { name: 'Stage' }))
    const bar = await screen.findByRole('region', { name: 'Pending network changes' })
    expect(within(bar).getByText('wg0')).toBeTruthy()
    expect(screen.getByText('Tunnel changes are staged and not applied yet.')).toBeTruthy()
  })
})
