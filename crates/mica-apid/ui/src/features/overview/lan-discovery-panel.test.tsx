import { cleanup, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { LanDiscoveryPanel } from './lan-discovery-panel'

const mdns = (enabled: boolean) => ({
  available: true,
  state: {
    enabled,
    source: 'product',
    hostname: 'mica-ab12.local',
    services: [
      { instance: 'mica-ab12', type: '_mica._tcp', port: 8080, txt: { txtvers: '1' }, addresses: [], source: 'built-in', published: enabled },
      { instance: 'Grafana', type: '_http._tcp', port: 3000, txt: {}, addresses: [], source: 'image:grafana', published: enabled },
    ],
    refused: [{ source: 'runtime:broken', error: 'EOF while parsing' }],
  },
})

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('the LAN discovery card', () => {
  it('lists every service with its source, and what is refused', async () => {
    stubFetch({ '/api/v1/mdns': mdns(true) })
    renderPanel(<LanDiscoveryPanel />)

    expect(await screen.findByText('mica-ab12._mica._tcp')).toBeTruthy()
    expect(screen.getByText('port 3000 · image:grafana')).toBeTruthy()
    expect(screen.getAllByText('Published')).toHaveLength(2)
    expect(screen.getByText(/mica-ab12\.local .* product default/)).toBeTruthy()
    expect(screen.getByText('Refused runtime:broken: EOF while parsing')).toBeTruthy()
  })

  it('lists the services as not published while it is off', async () => {
    stubFetch({ '/api/v1/mdns': mdns(false) })
    renderPanel(<LanDiscoveryPanel />)

    expect((await screen.findAllByText('Not published')).length).toBe(2)
    expect((await screen.findByRole('switch', { name: 'mDNS' })).getAttribute('aria-checked')).toBe('false')
  })

  it('writes the switch and reports it', async () => {
    const write = vi.fn(() => jsonResponse({ taskId: 'task-1' }, 202))
    stubFetch({ '/api/v1/mdns': mdns(true), 'PUT /api/v1/settings/access.mdns.enabled': write })
    renderPanel(<LanDiscoveryPanel />)

    await userEvent.click(await screen.findByRole('switch', { name: 'mDNS' }))

    expect((await screen.findAllByText('mDNS turned off.')).length).toBeGreaterThan(0)
    expect(write).toHaveBeenCalledOnce()
  })

  it('says why there is no state', async () => {
    stubFetch({ '/api/v1/mdns': { available: false, error: 'the mDNS reconciler has published no state' } })
    renderPanel(<LanDiscoveryPanel />)

    expect(await screen.findByText('the mDNS reconciler has published no state')).toBeTruthy()
    expect((await screen.findByRole('switch', { name: 'mDNS' })).hasAttribute('disabled') || (await screen.findByRole('switch', { name: 'mDNS' })).getAttribute('aria-disabled') === 'true').toBe(true)
  })
})
