import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { TimeStatus } from '@/lib/types'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { NtpServersPanel, SyncStatusPanel, TimezonePanel } from './time-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

/// The zone catalog of a device whose stored zone is `current`.
function zones(current: string, installed = true) {
  const ids = ['Europe/Berlin', 'Asia/Shanghai', 'America/Argentina/Buenos_Aires']
  return {
    tzdataVersion: '2026c',
    zones: [{ id: 'UTC', region: null, city: null }, ...ids.map((id) => ({ id, region: id.split('/')[0], city: id.split('/').slice(1).join('/') }))],
    current: { id: current, installed, listed: current === 'UTC' || ids.includes(current) },
  }
}

describe('time synchronization status', () => {
  it('reports a synchronized clock with the server and the last sample', async () => {
    stubFetch({
      '/api/v1/time/status': {
        status: 'synchronized',
        synchronized: true,
        server: { name: 'time.cloudflare.com', address: '162.159.200.1' },
        sample: { leap: 0, stratum: 3, spike: false, offsetSeconds: 0.0024, packetCount: 8, correction: 'slew' },
      } satisfies TimeStatus,
    })
    renderPanel(<SyncStatusPanel />)

    expect(await screen.findByText('synchronized — the kernel reports a bounded clock error')).toBeTruthy()
    expect(screen.getByText('time.cloudflare.com (162.159.200.1)')).toBeTruthy()
    expect(screen.getByText('stratum 3, offset 2.4 ms')).toBeTruthy()
    expect(screen.getByText('slewing (ordinary drift)')).toBeTruthy()
  })

  it('names the degraded state and says retries continue', async () => {
    stubFetch({ '/api/v1/time/status': { status: 'offline-degraded', synchronized: false } satisfies TimeStatus })
    renderPanel(<SyncStatusPanel />)

    expect(await screen.findByText(/degraded — no reachable time server/)).toBeTruthy()
    expect(screen.getByText(/keeps retrying every 30 seconds/)).toBeTruthy()
  })

  it('renders a signal that could not be read as absence with its reason', async () => {
    stubFetch({
      '/api/v1/time/status': {
        status: 'unknown',
        detail: 'systemd-timesyncd is not reachable on the bus',
      } satisfies TimeStatus,
    })
    renderPanel(<SyncStatusPanel />)

    expect(await screen.findByText('status unavailable — a signal this status rests on could not be read')).toBeTruthy()
    expect(screen.getByText('systemd-timesyncd is not reachable on the bus')).toBeTruthy()
  })

  // The other way this state is reached. The label cannot say which
  // service went missing -- it is one string for both -- so the reason line is
  // what an operator reads, and there is no `synchronized` member here at all:
  // a device that could not be queried must not render as one that was.
  it('says nothing about the clock when only the kernel bit went unread', async () => {
    stubFetch({
      '/api/v1/time/status': {
        status: 'unknown',
        detail: 'systemd-timedated did not answer NTPSynchronized: the kernel\'s bound on the clock error was not read',
        server: { name: '0.pool.ntp.org', address: '192.0.2.7' },
      } satisfies TimeStatus,
    })
    renderPanel(<SyncStatusPanel />)

    expect(await screen.findByText('status unavailable — a signal this status rests on could not be read')).toBeTruthy()
    expect(screen.getByText(/systemd-timedated did not answer NTPSynchronized/)).toBeTruthy()
    expect(screen.getByText('0.pool.ntp.org (192.0.2.7)')).toBeTruthy()
  })

  it('offers no pause or enable control for synchronization', async () => {
    stubFetch({ '/api/v1/time/status': { status: 'synchronized', synchronized: true } satisfies TimeStatus })
    renderPanel(<SyncStatusPanel />)

    await screen.findByText('synchronized — the kernel reports a bounded clock error')
    expect(screen.queryAllByRole('switch')).toHaveLength(0)
    expect(screen.queryAllByRole('button')).toHaveLength(0)
  })

  it('surfaces a read failure as an error', async () => {
    stubFetch({ '/api/v1/time/status': () => jsonResponse({ error: { message: 'micad timed out' } }, 504) })
    renderPanel(<SyncStatusPanel />)

    expect(await screen.findByText('micad timed out')).toBeTruthy()
  })
})

describe('time settings', () => {
  it('writes the server list one per line and follows the accepted task', async () => {
    let taskStatus = { id: 'task-9', dotPath: 'time.ntp.servers', status: 'running', foldedCount: 0 }
    const fetch = stubFetch({
      '/api/v1/settings/time.ntp.servers': ['0.pool.ntp.org'],
      'PUT /api/v1/settings/time.ntp.servers': () => jsonResponse({ taskId: 'task-9' }),
      '/api/v1/tasks/task-9': () => jsonResponse(taskStatus),
    })
    renderPanel(<NtpServersPanel />)

    const textarea = await screen.findByRole('textbox', { name: 'Servers' })
    await waitFor(() => expect((textarea as HTMLTextAreaElement).value).toBe('0.pool.ntp.org'))
    await userEvent.clear(textarea)
    await userEvent.type(textarea, 'time.example.com{enter}1.pool.ntp.org')
    await userEvent.click(screen.getByRole('button', { name: 'Save servers' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(true))
    const put = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')!
    expect(put[0]).toBe('/api/v1/settings/time.ntp.servers')
    expect((put[1] as RequestInit).body).toBe('["time.example.com","1.pool.ntp.org"]')

    expect(await screen.findByText('Applying time.ntp.servers…')).toBeTruthy()
    taskStatus = { ...taskStatus, status: 'finished', ...{ outcome: 'succeeded' } }
    expect(await screen.findByText('Change applied.', {}, { timeout: 4_000 })).toBeTruthy()
  })

  it('picks a zone by searching its name or region, and writes its IANA ID', async () => {
    const fetch = stubFetch({
      '/api/v1/settings/time.timezone': 'UTC',
      '/api/v1/time/zones': zones('UTC'),
      'PUT /api/v1/settings/time.timezone': () => jsonResponse({ taskId: 'task-3' }),
      '/api/v1/tasks/task-3': () => jsonResponse({ id: 'task-3', dotPath: 'time.timezone', status: 'finished', outcome: 'succeeded', foldedCount: 0 }),
    })
    renderPanel(<TimezonePanel />)

    expect(screen.getByText(/The device clock, logs and the API stay UTC/)).toBeTruthy()
    const input = await screen.findByRole('combobox', { name: /^Timezone/ })
    await waitFor(() => expect((input as HTMLInputElement).value).toMatch(/^UTC/))
    expect((screen.getByRole('button', { name: 'Save timezone' }) as HTMLButtonElement).disabled).toBe(true)

    await userEvent.clear(input)
    await userEvent.type(input, 'europe/ber')
    const option = await screen.findByRole('option', { name: /^Europe \/ Berlin · Europe\/Berlin · UTC\+0[12]:00$/ })
    expect(screen.queryByRole('option', { name: /Asia\/Shanghai/ })).toBeNull()
    await userEvent.click(option)
    await userEvent.click(screen.getByRole('button', { name: 'Save timezone' }))

    await waitFor(() => expect(fetch.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')).toBe(true))
    const put = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === 'PUT')!
    expect((put[1] as RequestInit).body).toBe('"Europe/Berlin"')
    expect(await screen.findByText('Change applied.')).toBeTruthy()
  })

  it('lists UTC first and the rest by region, with the offset each has now', async () => {
    stubFetch({ '/api/v1/settings/time.timezone': 'UTC', '/api/v1/time/zones': zones('UTC') })
    renderPanel(<TimezonePanel />)

    const input = await screen.findByRole('combobox', { name: /^Timezone/ })
    await userEvent.clear(input)
    await userEvent.type(input, 'utc')
    const options = await screen.findAllByRole('option')
    expect(options[0].textContent).toMatch(/^UTC · UTC\+00:00/)
    expect(screen.getByRole('option', { name: /^Asia \/ Shanghai · Asia\/Shanghai · UTC\+08:00$/ })).toBeTruthy()
    expect(screen.getByRole('option', { name: /^America \/ Argentina \/ Buenos Aires · America\/Argentina\/Buenos_Aires · UTC-03:00$/ })).toBeTruthy()
  })

  it('names a compatible zone as one, and warns of a zone the device does not carry', async () => {
    stubFetch({ '/api/v1/settings/time.timezone': 'Arctic/Longyearbyen', '/api/v1/time/zones': zones('Arctic/Longyearbyen', true) })
    renderPanel(<TimezonePanel />)
    expect(await screen.findByText(/Arctic\/Longyearbyen is a compatibility name/)).toBeTruthy()
    cleanup()

    stubFetch({ '/api/v1/settings/time.timezone': 'Atlantis/Made_Up', '/api/v1/time/zones': zones('Atlantis/Made_Up', false) })
    renderPanel(<TimezonePanel />)
    expect(await screen.findByText(/Atlantis\/Made_Up is not a zone this device carries/)).toBeTruthy()
  })

  it('keeps the stored zone on screen and offers a retry when the catalog cannot be read', async () => {
    let reads = 0
    stubFetch({
      '/api/v1/settings/time.timezone': 'Europe/Berlin',
      '/api/v1/time/zones': () => {
        reads += 1
        return reads === 1
          ? jsonResponse({ error: { code: 'tzdata_unavailable', message: "this device's tzdata has no zone table" } }, 503)
          : jsonResponse(zones('Europe/Berlin'))
      },
    })
    renderPanel(<TimezonePanel />)

    expect(await screen.findByText(/this device's tzdata has no zone table/)).toBeTruthy()
    expect(screen.getByText('Europe/Berlin')).toBeTruthy()
    expect(screen.queryByRole('combobox', { name: /^Timezone/ })).toBeNull()
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }))
    expect(await screen.findByRole('combobox', { name: /^Timezone/ })).toBeTruthy()
  })

  it('surfaces a rejected write and keeps the choice', async () => {
    stubFetch({
      '/api/v1/settings/time.timezone': 'UTC',
      '/api/v1/time/zones': zones('UTC'),
      'PUT /api/v1/settings/time.timezone': () => jsonResponse({ error: { message: 'timezone_invalid: `Europe/Berlin` is not a zone in this device\'s tzdata' } }, 422),
    })
    renderPanel(<TimezonePanel />)

    const input = await screen.findByRole('combobox', { name: /^Timezone/ })
    await userEvent.clear(input)
    await userEvent.type(input, 'berlin')
    await userEvent.click(await screen.findByRole('option', { name: /Europe\/Berlin/ }))
    await userEvent.click(screen.getByRole('button', { name: 'Save timezone' }))

    expect(await screen.findByText(/is not a zone in this device's tzdata/)).toBeTruthy()
    expect((input as HTMLInputElement).value).toMatch(/Europe\/Berlin/)
  })
})
